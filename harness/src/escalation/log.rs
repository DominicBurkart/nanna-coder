use super::{Escalation, EscalationError};
use crate::scheduler::default_queue_path;
use crate::telemetry::TelemetrySystem;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Environment variable overriding the escalation log location.
pub const ESCALATION_PATH_ENV: &str = "NANNA_ESCALATION_PATH";

/// Location of the escalation log for this user: `NANNA_ESCALATION_PATH`
/// when set, otherwise `escalations.jsonl` next to the queue log
/// ([`default_queue_path`]), or `None` when neither is available.
pub fn default_escalation_path() -> Option<PathBuf> {
    escalation_path_from(std::env::var_os(ESCALATION_PATH_ENV), default_queue_path())
}

/// Pure form of [`default_escalation_path`].
///
/// ```
/// use harness::escalation::escalation_path_from;
/// use std::path::PathBuf;
///
/// assert_eq!(escalation_path_from(Some("/var/lib/nanna/e.jsonl".into()), None), Some(PathBuf::from("/var/lib/nanna/e.jsonl")));
/// assert_eq!(escalation_path_from(None, Some(PathBuf::from("/home/u/.local/state/nanna/queue.jsonl"))), Some(PathBuf::from("/home/u/.local/state/nanna/escalations.jsonl")));
/// assert_eq!(escalation_path_from(None, None), None);
/// ```
pub fn escalation_path_from(
    override_path: Option<std::ffi::OsString>,
    queue_path: Option<PathBuf>,
) -> Option<PathBuf> {
    override_path
        .map(PathBuf::from)
        .or_else(|| queue_path.map(|q| q.with_file_name("escalations.jsonl")))
}

/// Production-class work for `repo` is parked while this hold exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentHold {
    /// Id of the `incident` escalation that set the hold; `nanna escalation
    /// resolve <id>` clears it.
    pub escalation_id: String,
    pub repo: String,
    pub summary: String,
    pub since: DateTime<Utc>,
}

impl IncidentHold {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "escalation_id": self.escalation_id, "repo": self.repo, "summary": self.summary, "since": self.since.to_rfc3339() })
    }
}

/// What the log knows about one dedupe key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyState {
    pub key: String,
    /// Id of the most recent escalation with this key.
    pub last_id: String,
    /// Occurrences seen, delivered or collapsed.
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// When a sink last accepted this key; `None` until the first delivery.
    pub last_delivered: Option<DateTime<Utc>>,
}

/// Rate-limit decision for one more occurrence of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    /// Ordinal of this occurrence, starting at 1.
    pub number: u64,
    /// Whether the sinks should see it, or it collapses into the counter.
    pub deliver: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Record {
    Seen { state: KeyState },
    Hold { hold: IncidentHold },
    Resolve { escalation_id: String },
}

#[derive(Debug, Clone, Default)]
struct LogTable {
    keys: BTreeMap<String, KeyState>,
    holds: BTreeMap<String, IncidentHold>,
}

impl LogTable {
    fn apply(&mut self, record: Record) {
        match record {
            Record::Seen { state } => {
                self.keys.insert(state.key.clone(), state);
            }
            Record::Hold { hold } => {
                self.holds.insert(hold.escalation_id.clone(), hold);
            }
            Record::Resolve { escalation_id } => {
                self.holds.remove(&escalation_id);
            }
        }
    }

    fn records(&self) -> Vec<Record> {
        let seen = self
            .keys
            .values()
            .map(|s| Record::Seen { state: s.clone() });
        let holds = self
            .holds
            .values()
            .map(|h| Record::Hold { hold: h.clone() });
        seen.chain(holds).collect()
    }
}

/// Durable record of every escalation key and every incident hold.
///
/// Persisted as JSON Lines like the queue and lease logs: one record per
/// mutation, replayed and compacted on [`open`](Self::open); every mutation
/// is appended before the in-memory table adopts it. [`in_memory`](Self::in_memory)
/// gives a log with no file for tests and the default [`crate::task::TaskManager`].
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::escalation::{Escalation, EscalationLog, EscalationSource, Severity};
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("escalations.jsonl");
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let incident = Escalation::new(Severity::Incident, EscalationSource::Rollout, "example/repo", "p99 breached").with_id("inc-1");
///
/// let log = EscalationLog::open(&path).unwrap();
/// log.hold(&incident, t0).unwrap();
/// assert!(log.production_held("example/repo"));
///
/// let reopened = EscalationLog::open(&path).unwrap();
/// assert!(reopened.production_held("example/repo"));
/// assert_eq!(reopened.resolve("inc-1", t0 + Duration::hours(1)).unwrap().repo, "example/repo");
/// assert!(!EscalationLog::open(&path).unwrap().production_held("example/repo"));
/// ```
#[derive(Debug, Clone)]
pub struct EscalationLog {
    path: Option<PathBuf>,
    table: Arc<Mutex<LogTable>>,
}

impl EscalationLog {
    /// A log kept only in process memory. Clones share the same table.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            table: Arc::new(Mutex::new(LogTable::default())),
        }
    }

    /// Open or create the log at `path`, creating parent directories,
    /// replay it and rewrite it with one record per key and live hold.
    pub fn open(path: &Path) -> Result<Self, EscalationError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(path)?;
        let table = replay(path)?;
        fs::write(path, lines(&table.records())?)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            table: Arc::new(Mutex::new(table)),
        })
    }

    /// Location of the log file, if it has one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn commit(&self, record: Record) -> Result<(), EscalationError> {
        let mut table = self.table.lock().unwrap();
        if let Some(path) = &self.path {
            let mut file = OpenOptions::new().append(true).open(path)?;
            file.write_all(lines(&[record_ref(&record)])?.as_bytes())?;
            file.flush()?;
        }
        table.apply(record);
        Ok(())
    }

    /// Whether one more occurrence of `key` at `now` should reach the
    /// sinks: yes for a key never delivered, or whose last delivery is at
    /// least `window` ago; otherwise it collapses into the counter.
    pub fn occurrence(&self, key: &str, now: DateTime<Utc>, window: Duration) -> Occurrence {
        let table = self.table.lock().unwrap();
        let Some(state) = table.keys.get(key) else {
            return Occurrence {
                number: 1,
                deliver: true,
            };
        };
        let deliver = state.last_delivered.is_none_or(|at| now - at >= window);
        Occurrence {
            number: state.count + 1,
            deliver,
        }
    }

    /// Count one occurrence of `escalation` at `now`; `delivered` says
    /// whether a sink accepted it. Returns the updated state.
    pub fn record(
        &self,
        escalation: &Escalation,
        now: DateTime<Utc>,
        delivered: bool,
    ) -> Result<KeyState, EscalationError> {
        let key = escalation.dedupe_key();
        let previous = self.table.lock().unwrap().keys.get(&key).cloned();
        let mut state = previous.unwrap_or(KeyState {
            key,
            last_id: escalation.id.clone(),
            count: 0,
            first_seen: now,
            last_seen: now,
            last_delivered: None,
        });
        state.count += 1;
        state.last_id = escalation.id.clone();
        state.last_seen = now;
        if delivered {
            state.last_delivered = Some(now);
        }
        self.commit(Record::Seen {
            state: state.clone(),
        })?;
        Ok(state)
    }

    /// Park production-class work for `escalation.repo` until a human
    /// resolves `escalation.id`.
    pub fn hold(
        &self,
        escalation: &Escalation,
        now: DateTime<Utc>,
    ) -> Result<IncidentHold, EscalationError> {
        let hold = IncidentHold {
            escalation_id: escalation.id.clone(),
            repo: escalation.repo.clone(),
            summary: escalation.headline(),
            since: now,
        };
        self.commit(Record::Hold { hold: hold.clone() })?;
        tracing::warn!(repo = %hold.repo, escalation = %hold.escalation_id, "Incident hold set: production work parked");
        Ok(hold)
    }

    /// Clear the hold set by `escalation_id`. Only humans call this, via
    /// `nanna escalation resolve`; no agent tool reaches it.
    pub fn resolve(
        &self,
        escalation_id: &str,
        now: DateTime<Utc>,
    ) -> Result<IncidentHold, EscalationError> {
        let hold = self.table.lock().unwrap().holds.get(escalation_id).cloned();
        let hold = hold.ok_or_else(|| EscalationError::UnknownHold(escalation_id.to_string()))?;
        self.commit(Record::Resolve {
            escalation_id: escalation_id.to_string(),
        })?;
        tracing::info!(repo = %hold.repo, escalation = escalation_id, held_for = %(now - hold.since), "Incident hold resolved");
        Ok(hold)
    }

    /// Whether any incident hold parks production work for `repo`.
    pub fn production_held(&self, repo: &str) -> bool {
        self.table
            .lock()
            .unwrap()
            .holds
            .values()
            .any(|h| h.repo == repo)
    }

    /// Every live hold, by escalation id.
    pub fn holds(&self) -> Vec<IncidentHold> {
        self.table.lock().unwrap().holds.values().cloned().collect()
    }

    /// Every tracked key, in key order.
    pub fn states(&self) -> Vec<KeyState> {
        self.table.lock().unwrap().keys.values().cloned().collect()
    }

    /// Counts and holds as of `now`.
    pub fn snapshot(&self, now: DateTime<Utc>) -> EscalationSnapshot {
        let states = self.states();
        EscalationSnapshot {
            tracked: states.len(),
            occurrences: states.iter().map(|s| s.count).sum(),
            holds: self.holds(),
            at: now,
        }
    }
}

fn record_ref(record: &Record) -> Record {
    match record {
        Record::Seen { state } => Record::Seen {
            state: state.clone(),
        },
        Record::Hold { hold } => Record::Hold { hold: hold.clone() },
        Record::Resolve { escalation_id } => Record::Resolve {
            escalation_id: escalation_id.clone(),
        },
    }
}

fn lines(records: &[Record]) -> Result<String, EscalationError> {
    let mut out = String::new();
    for record in records {
        out.push_str(&serde_json::to_string(record)?);
        out.push('\n');
    }
    Ok(out)
}

fn replay(path: &Path) -> Result<LogTable, EscalationError> {
    let mut table = LogTable::default();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        table.apply(serde_json::from_str(&line)?);
    }
    Ok(table)
}

/// Point-in-time view of the log for `tasks/list` metadata, `nanna health`
/// and telemetry.
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use harness::escalation::{Escalation, EscalationLog, EscalationSource, Severity};
/// use harness::telemetry::TelemetrySystem;
///
/// let log = EscalationLog::in_memory();
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let incident = Escalation::new(Severity::Incident, EscalationSource::Incident, "example/repo", "down").with_id("inc-1");
/// log.record(&incident, t0, true).unwrap();
/// log.hold(&incident, t0).unwrap();
///
/// let snapshot = log.snapshot(t0);
/// assert_eq!(snapshot.to_string(), "tracked=1 occurrences=1 holds=1");
/// assert_eq!(snapshot.to_json()["production_held"][0], "example/repo");
/// let telemetry = TelemetrySystem::new();
/// snapshot.record(&telemetry);
/// assert_eq!(telemetry.get_buffered_metrics_count(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationSnapshot {
    /// Distinct dedupe keys seen.
    pub tracked: usize,
    /// Occurrences across every key, collapsed ones included.
    pub occurrences: u64,
    /// Live incident holds.
    pub holds: Vec<IncidentHold>,
    pub at: DateTime<Utc>,
}

impl EscalationSnapshot {
    /// Repositories with production work parked, deduplicated and sorted.
    pub fn production_held(&self) -> Vec<String> {
        let mut repos: Vec<String> = self.holds.iter().map(|h| h.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        repos
    }

    /// Record the counts as gauges on `telemetry`.
    pub fn record(&self, telemetry: &TelemetrySystem) {
        telemetry.record_gauge("nanna_escalations_tracked", self.tracked as f64, vec![]);
        telemetry.record_gauge("nanna_incident_holds", self.holds.len() as f64, vec![]);
    }

    /// JSON form for health output and MCP metadata.
    pub fn to_json(&self) -> serde_json::Value {
        let holds: Vec<serde_json::Value> = self.holds.iter().map(IncidentHold::to_json).collect();
        serde_json::json!({ "tracked": self.tracked, "occurrences": self.occurrences, "holds": holds, "production_held": self.production_held(), "at": self.at.to_rfc3339() })
    }
}

impl fmt::Display for EscalationSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tracked={} occurrences={} holds={}",
            self.tracked,
            self.occurrences,
            self.holds.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::{EscalationSource, Severity};
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn incident(id: &str, repo: &str) -> Escalation {
        Escalation::new(
            Severity::Incident,
            EscalationSource::Incident,
            repo,
            format!("outage {id}\nmore"),
        )
        .with_id(id)
    }

    #[test]
    fn occurrence_counts_and_window_gate_delivery() {
        let log = EscalationLog::in_memory();
        assert!(log.path().is_none());
        let e = incident("a", "example/repo");
        let key = e.dedupe_key();
        let window = Duration::minutes(10);
        assert_eq!(
            log.occurrence(&key, t0(), window),
            Occurrence {
                number: 1,
                deliver: true
            }
        );
        let state = log.record(&e, t0(), false).unwrap();
        assert_eq!((state.count, state.last_delivered), (1, None));
        assert_eq!(
            log.occurrence(&key, t0() + Duration::minutes(1), window),
            Occurrence {
                number: 2,
                deliver: true
            }
        );
        log.record(&e, t0() + Duration::minutes(1), true).unwrap();
        assert_eq!(
            log.occurrence(&key, t0() + Duration::minutes(5), window),
            Occurrence {
                number: 3,
                deliver: false
            }
        );
        let state = log
            .record(&e.clone().with_id("b"), t0() + Duration::minutes(5), false)
            .unwrap();
        assert_eq!(
            (state.count, state.last_id.as_str(), state.first_seen),
            (3, "b", t0())
        );
        assert_eq!(state.last_delivered, Some(t0() + Duration::minutes(1)));
        assert_eq!(
            log.occurrence(&key, t0() + Duration::minutes(11), window),
            Occurrence {
                number: 4,
                deliver: true
            }
        );
        assert_eq!(
            log.occurrence("other", t0(), window),
            Occurrence {
                number: 1,
                deliver: true
            }
        );
        assert_eq!(
            log.snapshot(t0()).to_string(),
            "tracked=1 occurrences=3 holds=0"
        );
    }

    #[test]
    fn holds_are_per_repo_and_cleared_only_by_resolve() {
        let log = EscalationLog::in_memory();
        let hold = log.hold(&incident("inc-1", "example/repo"), t0()).unwrap();
        assert_eq!(
            hold,
            IncidentHold {
                escalation_id: "inc-1".into(),
                repo: "example/repo".into(),
                summary: "outage inc-1".into(),
                since: t0()
            }
        );
        log.hold(&incident("inc-2", "example/repo"), t0()).unwrap();
        assert!(log.production_held("example/repo"));
        assert!(!log.production_held("example/other"));
        assert_eq!(log.holds().len(), 2);
        let snapshot = log.snapshot(t0());
        assert_eq!(snapshot.production_held(), vec!["example/repo".to_string()]);
        assert_eq!(snapshot.to_json()["holds"][1]["escalation_id"], "inc-2");
        assert_eq!(
            snapshot.to_json()["holds"][0]["since"],
            "2026-09-24T12:00:00+00:00"
        );
        assert_eq!(log.resolve("inc-1", t0()).unwrap().escalation_id, "inc-1");
        assert!(log.production_held("example/repo"));
        let err = log.resolve("inc-1", t0()).unwrap_err();
        assert_eq!(err.to_string(), "no incident hold with id `inc-1`");
        log.resolve("inc-2", t0()).unwrap();
        assert!(!log.production_held("example/repo"));
        assert!(log.clone().holds().is_empty());
    }

    #[test]
    fn jsonl_round_trip_replays_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("escalations.jsonl");
        let log = EscalationLog::open(&path).unwrap();
        assert_eq!(log.path(), Some(path.as_path()));
        let a = incident("a", "example/repo");
        log.record(&a, t0(), true).unwrap();
        log.record(&a, t0() + Duration::minutes(1), false).unwrap();
        log.hold(&a, t0()).unwrap();
        log.hold(&incident("b", "example/other"), t0()).unwrap();
        log.resolve("b", t0()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 5);

        let reopened = EscalationLog::open(&path).unwrap();
        assert_eq!(reopened.states(), log.states());
        assert_eq!(reopened.holds(), log.holds());
        assert_eq!(reopened.states()[0].count, 2);
        assert!(reopened.production_held("example/repo"));
        assert!(!reopened.production_held("example/other"));
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
    }

    #[test]
    fn skips_blank_lines_rejects_garbage_and_surfaces_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.jsonl");
        fs::write(&path, "\n\n").unwrap();
        assert!(EscalationLog::open(&path).unwrap().states().is_empty());
        fs::write(&path, "{not json}\n").unwrap();
        let err = EscalationLog::open(&path).unwrap_err();
        assert!(matches!(err, EscalationError::Serde(_)));
        assert!(err.to_string().contains("not valid JSON"));

        fs::write(&path, "").unwrap();
        let log = EscalationLog::open(&path).unwrap();
        let a = incident("a", "example/repo");
        log.hold(&a, t0()).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            log.record(&a, t0(), true),
            Err(EscalationError::Io(_))
        ));
        assert!(matches!(
            log.resolve("a", t0()),
            Err(EscalationError::Io(_))
        ));
        assert!(log.production_held("example/repo"));
        assert!(log.states().is_empty());
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        assert!(EscalationLog::open(&blocker.join("x"))
            .unwrap_err()
            .to_string()
            .contains("I/O error"));
    }

    #[test]
    fn default_escalation_path_reads_environment() {
        let resolved = default_escalation_path();
        let expected =
            escalation_path_from(std::env::var_os(ESCALATION_PATH_ENV), default_queue_path());
        assert_eq!(resolved, expected);
    }
}
