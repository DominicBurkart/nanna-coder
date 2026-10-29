use super::{DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink};
use async_trait::async_trait;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT_RECORD_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

/// Appends [`Escalation::to_json`] (redacted) as one JSON line per delivery,
/// so every escalation leaves content a human can read even when no remote
/// sink is configured. Writes stop with [`EscalationError::RecordFull`] once
/// the file would exceed `limit_bytes`, which bounds its growth.
///
/// ```
/// use harness::escalation::{Escalation, EscalationSink, EscalationSource, LocalRecordSink, Severity};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let sink = LocalRecordSink::new(dir.path().join("records.jsonl"), 1 << 20);
/// let escalation = Escalation::new(Severity::Info, EscalationSource::Manual, "example/repo", "hello");
/// sink.deliver(&escalation).await.unwrap();
/// assert_eq!(std::fs::read_to_string(sink.path()).unwrap().lines().count(), 1);
/// # });
/// ```
pub struct LocalRecordSink {
    path: PathBuf,
    limit_bytes: u64,
    write: Mutex<()>,
}

impl LocalRecordSink {
    pub fn new(path: impl Into<PathBuf>, limit_bytes: u64) -> Self {
        Self {
            path: path.into(),
            limit_bytes,
            write: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[async_trait]
impl EscalationSink for LocalRecordSink {
    fn name(&self) -> &str {
        "local-record"
    }

    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
        let mut line = serde_json::to_string(&escalation.to_json())?;
        line.push('\n');
        let _guard = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let used = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        if used.saturating_add(line.len() as u64) > self.limit_bytes {
            return Err(EscalationError::RecordFull {
                limit_bytes: self.limit_bytes,
            });
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&self.path)?.write_all(line.as_bytes())?;
        Ok(DeliveryReceipt {
            sink: self.name().to_string(),
            reference: self.path.display().to_string(),
            outcome: DeliveryOutcome::Recorded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::{EscalationSource, Severity};

    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    fn escalation(summary: &str) -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            "example/repo",
            summary,
        )
        .with_evidence(vec![format!("Authorization: Bearer {TOKEN}")])
    }

    #[tokio::test]
    async fn each_delivery_appends_one_redacted_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.jsonl");
        let sink = LocalRecordSink::new(&path, 1 << 20);
        assert_eq!(sink.path(), path);
        let first = sink.deliver(&escalation("first")).await.unwrap();
        sink.deliver(&escalation("second")).await.unwrap();
        assert_eq!(first.outcome, DeliveryOutcome::Recorded);
        assert_eq!(first.reference, path.display().to_string());
        assert_eq!(first.sink, "local-record");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(TOKEN), "{text}");
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["summary"], "first");
        assert_eq!(lines[1]["summary"], "second");
        assert_eq!(lines[0]["repo"], "example/repo");
        assert!(lines[0]["body"].as_str().unwrap().contains("## Evidence"));
        assert!(lines[0]["dedupe_key"]
            .as_str()
            .unwrap()
            .starts_with("auditor:example/repo:"));
    }

    #[tokio::test]
    async fn an_existing_record_is_appended_to_never_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.jsonl");
        std::fs::write(&path, "earlier\n").unwrap();
        LocalRecordSink::new(&path, 1 << 20)
            .deliver(&escalation("later"))
            .await
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("earlier\n"), "{text}");
        assert!(text.contains("later"));
    }

    #[tokio::test]
    async fn a_full_record_refuses_the_write_and_stays_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.jsonl");
        let one = LocalRecordSink::new(&path, 1 << 20);
        one.deliver(&escalation("a")).await.unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        let exact = LocalRecordSink::new(&path, size * 2);
        exact.deliver(&escalation("b")).await.unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size * 2);

        let tight = LocalRecordSink::new(&path, size * 2);
        let err = tight.deliver(&escalation("c")).await.unwrap_err();
        assert!(
            matches!(err, EscalationError::RecordFull { limit_bytes } if limit_bytes == size * 2),
            "{err}"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size * 2);
    }

    #[tokio::test]
    async fn an_unwritable_location_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LocalRecordSink::new(dir.path().join("missing").join("records.jsonl"), 1 << 20);
        let err = sink.deliver(&escalation("x")).await.unwrap_err();
        assert!(matches!(err, EscalationError::Io(_)), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_record_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.jsonl");
        LocalRecordSink::new(&path, 1 << 20)
            .deliver(&escalation("x"))
            .await
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
