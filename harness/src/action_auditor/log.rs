//! Append-only record of every reviewed action.
//!
//! A sibling of [`crate::auditor::AuditLog`] rather than a reuse of it:
//! that log is typed to [`SpawnRequest`](crate::auditor::SpawnRequest)/
//! [`SpawnVerdict`](crate::auditor::SpawnVerdict), and an [`ActionReview`]/
//! [`ActionVerdict`] pair is a different record shape (a tool call, not a
//! spawn), so it gets its own log type with the same append-only contract.

use super::{is_message_tool, ActionAuditError, ActionReview, ActionVerdict};
use crate::auditor::content_hash;
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

const MAX_LOGGED_KEYS: usize = 16;
const MAX_LOGGED_KEY_CHARS: usize = 32;

fn loggable_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key.chars().count() <= MAX_LOGGED_KEY_CHARS
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        key.to_string()
    } else {
        "?".to_string()
    }
}

/// Replace what a message tool's arguments say with a record of their shape.
///
/// The arguments of `msg_send`, `session_set_note` and `sessions_query` are
/// the message, note or query itself, so writing them to the audit trail
/// would store in plaintext the very content the audit exists to keep out.
/// For those tools the result keeps top-level key names (only when they are
/// short identifiers), the serialized byte length and a content hash; any
/// other tool's review is returned unchanged.
///
/// ```
/// use harness::action_auditor::{redact_for_log, ActionReview};
/// use harness::effects::EffectClass;
/// use harness::task::TaskId;
///
/// let mut review = ActionReview {
///     identity: "a".to_string(),
///     task_id: TaskId("t".to_string()),
///     tool: "msg_send".to_string(),
///     args: serde_json::json!({"body": "secret text"}),
///     effect_class: EffectClass::None,
///     prior_actions: vec![],
/// };
/// let redacted = redact_for_log(&review);
/// assert!(!serde_json::to_string(&redacted).unwrap().contains("secret"));
/// assert_eq!(redacted.args["keys"][0], "body");
///
/// review.tool = "write_file".to_string();
/// assert_eq!(redact_for_log(&review), review);
/// ```
pub fn redact_for_log(review: &ActionReview) -> ActionReview {
    if !is_message_tool(&review.tool) {
        return review.clone();
    }
    let serialized = serde_json::to_string(&review.args).unwrap_or_default();
    let keys: Vec<String> = match &review.args {
        serde_json::Value::Object(map) => map
            .keys()
            .take(MAX_LOGGED_KEYS)
            .map(|k| loggable_key(k))
            .collect(),
        _ => Vec::new(),
    };
    let mut redacted = review.clone();
    redacted.args = serde_json::json!({
        "redacted": true,
        "keys": keys,
        "bytes": serialized.len(),
        "hash": content_hash(&serialized),
    });
    redacted
}

static APPEND_LOCK: Mutex<()> = Mutex::new(());

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

    /// The file entries are appended to, or `None` for an in-memory log.
    pub fn path(&self) -> Option<&std::path::Path> {
        match &self.backing {
            Backing::Memory(_) => None,
            Backing::File(path) => Some(path),
        }
    }

    /// Append `verdict` for `review`.
    pub fn append(
        &self,
        review: &ActionReview,
        verdict: &ActionVerdict,
    ) -> Result<(), ActionAuditError> {
        let entry = ActionAuditLogEntry {
            review: redact_for_log(review),
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
                let mut line = serde_json::to_string(&entry)?;
                line.push('\n');
                let _serialized = APPEND_LOCK.lock();
                let mut file = OpenOptions::new().create(true).append(true).open(path)?;
                file.write_all(line.as_bytes())?;
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
    fn concurrent_appends_keep_every_line_intact() {
        let dir = tempfile::tempdir().unwrap();
        let log = ActionAuditLog::file(dir.path().join("audit.jsonl"));
        let mut big = review();
        big.args = serde_json::json!({ "blob": "x".repeat(4096) });
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        log.append(&big, &ActionVerdict::Allow).unwrap();
                    }
                });
            }
        });
        assert_eq!(log.entries().unwrap().len(), 16 * 25);
    }

    #[test]
    fn path_is_the_file_for_a_file_log_and_none_in_memory() {
        assert_eq!(ActionAuditLog::in_memory().path(), None);
        let log = ActionAuditLog::file("/x/audit.jsonl");
        assert_eq!(log.path(), Some(std::path::Path::new("/x/audit.jsonl")));
    }

    #[test]
    fn in_memory_log_keeps_appending_after_a_poisoned_lock() {
        let log = ActionAuditLog::in_memory();
        let Backing::Memory(entries) = &log.backing else {
            unreachable!("in_memory builds a memory backing");
        };
        let poisoner = Arc::clone(entries);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the audit log lock");
        })
        .join();
        assert!(entries.is_poisoned());
        log.append(&review(), &ActionVerdict::Allow).unwrap();
        assert_eq!(log.entries().unwrap().len(), 1);
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

    const PLANTED: &str = "ghp_FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE0123";

    fn message_review(tool: &str, args: serde_json::Value) -> ActionReview {
        ActionReview {
            identity: "a".to_string(),
            task_id: TaskId("t".to_string()),
            tool: tool.to_string(),
            args,
            effect_class: EffectClass::None,
            prior_actions: vec![],
        }
    }

    #[test]
    fn a_message_tool_review_is_logged_without_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("action_audit.jsonl");
        for log in [ActionAuditLog::file(&path), ActionAuditLog::in_memory()] {
            for tool in crate::action_auditor::MESSAGE_TOOLS {
                let args = serde_json::json!({
                    "to": "peer",
                    "body": format!("here is my token {PLANTED}"),
                    PLANTED: "key names are content too",
                });
                log.append(&message_review(tool, args), &ActionVerdict::Allow)
                    .unwrap();
            }
            let entries = log.entries().unwrap();
            assert_eq!(entries.len(), 3);
            let shown = serde_json::to_string(&entries).unwrap();
            assert!(!shown.contains(PLANTED) && !shown.contains("my token"));
            assert_eq!(entries[0].review.args["redacted"], true);
            assert!(entries[0].review.args["bytes"].as_u64().unwrap() > 0);
            assert_eq!(entries[0].review.args["hash"].as_str().unwrap().len(), 16);
        }
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(PLANTED) && !on_disk.contains("token"));
    }

    #[test]
    fn redaction_keeps_short_identifier_keys_and_masks_the_rest() {
        let args = serde_json::json!({"body": "x", "a b": "y", "": "z"});
        let r = redact_for_log(&message_review("msg_send", args));
        let keys: Vec<&str> = r.args["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert!(keys.contains(&"body"));
        assert_eq!(keys.iter().filter(|k| **k == "?").count(), 2);
    }

    #[test]
    fn non_object_and_empty_message_args_are_redacted_too() {
        for args in [
            serde_json::json!(null),
            serde_json::json!("just a string secret"),
            serde_json::json!([PLANTED]),
            serde_json::json!({}),
        ] {
            let r = redact_for_log(&message_review("sessions_query", args));
            assert!(!serde_json::to_string(&r).unwrap().contains("secret"));
            assert!(!serde_json::to_string(&r).unwrap().contains(PLANTED));
            assert_eq!(r.args["redacted"], true);
            assert!(r.args["keys"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn every_other_tools_entries_stay_byte_identical() {
        let corpus = [
            (
                "write_file",
                serde_json::json!({"path": "a.rs", "content": PLANTED}),
            ),
            ("run_command", serde_json::json!({"command": "ls -la"})),
            ("msg_recv", serde_json::json!({"max": 10})),
            ("sessions_list", serde_json::json!({})),
            ("github_pr_status", serde_json::json!(null)),
            ("sandbox_deploy", serde_json::json!({"image_tag": "v2"})),
            ("msg_send_extra", serde_json::json!({"body": "x"})),
            ("MSG_SEND", serde_json::json!({"body": "x"})),
            ("", serde_json::json!([1, 2, 3])),
        ];
        let log = ActionAuditLog::in_memory();
        for (tool, args) in corpus {
            let review = message_review(tool, args);
            let expected = serde_json::to_string(&ActionAuditLogEntry {
                review: review.clone(),
                verdict: ActionVerdict::Allow,
            })
            .unwrap();
            assert_eq!(redact_for_log(&review), review, "{tool}");
            log.append(&review, &ActionVerdict::Allow).unwrap();
            let got = log.entries().unwrap().pop().unwrap();
            assert_eq!(serde_json::to_string(&got).unwrap(), expected, "{tool}");
        }
    }

    proptest::proptest! {
        #[test]
        fn no_message_body_ever_reaches_the_logged_line(body in "[A-Za-z0-9]{24,64}") {
            for tool in crate::action_auditor::MESSAGE_TOOLS {
                let r = redact_for_log(&message_review(tool, serde_json::json!({"body": body.clone()})));
                proptest::prop_assert!(!serde_json::to_string(&r).unwrap().contains(&body));
            }
        }
    }
}
