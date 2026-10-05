use super::state::{RolloutRecord, RolloutState};
use super::RolloutError;
use crate::scheduler::default_queue_path;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Environment variable overriding the rollout log location.
pub const ROLLOUT_PATH_ENV: &str = "NANNA_ROLLOUT_PATH";

/// Location of the rollout log for this user: `NANNA_ROLLOUT_PATH` when
/// set, otherwise `rollouts.jsonl` next to the queue log, or `None` when
/// neither is available.
pub fn default_rollout_path() -> Option<PathBuf> {
    rollout_path_from(std::env::var_os(ROLLOUT_PATH_ENV), default_queue_path())
}

/// Pure form of [`default_rollout_path`].
///
/// ```
/// use harness::rollout::rollout_path_from;
/// use std::path::PathBuf;
///
/// assert_eq!(
///     rollout_path_from(Some("/var/lib/nanna/r.jsonl".into()), None),
///     Some(PathBuf::from("/var/lib/nanna/r.jsonl"))
/// );
/// assert_eq!(
///     rollout_path_from(None, Some(PathBuf::from("/home/u/.local/state/nanna/queue.jsonl"))),
///     Some(PathBuf::from("/home/u/.local/state/nanna/rollouts.jsonl"))
/// );
/// assert_eq!(rollout_path_from(None, None), None);
/// ```
pub fn rollout_path_from(
    override_path: Option<std::ffi::OsString>,
    queue_path: Option<PathBuf>,
) -> Option<PathBuf> {
    override_path
        .map(PathBuf::from)
        .or_else(|| queue_path.map(|q| q.with_file_name("rollouts.jsonl")))
}

/// One persisted transition: the full record after it, and the state before.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloutTransition {
    /// When the transition was recorded.
    pub at: DateTime<Utc>,
    /// State before, `None` for the record's creation.
    pub from: Option<RolloutState>,
    /// The record after the transition.
    pub record: RolloutRecord,
}

impl RolloutTransition {
    /// One line for the CLI: when, from which state, to which, at what traffic.
    pub fn summary(&self) -> String {
        let from = self
            .from
            .as_ref()
            .map_or_else(|| "created".to_string(), ToString::to_string);
        let retained = self
            .record
            .retained_slot
            .as_ref()
            .map_or_else(String::new, |slot| format!("  retained: {slot}"));
        format!(
            "{}  {from} -> {}  traffic {}%{retained}",
            self.at, self.record.state, self.record.traffic_percent
        )
    }
}

/// Append-only JSON Lines log of rollout transitions: one
/// [`RolloutTransition`] per line. The latest line per id is the rollout's
/// current record, which is what a restarted executor resumes from.
///
/// ```
/// use chrono::Utc;
/// use harness::deploy::DeployTemplate;
/// use harness::rollout::{RolloutLog, RolloutRecord, RolloutState};
///
/// let dir = tempfile::tempdir().unwrap();
/// let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
/// let plan = DeployTemplate::parse(
///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
/// )
/// .unwrap()
/// .plan("sandbox")
/// .unwrap();
/// let now = Utc::now();
/// let mut record = RolloutRecord::new("rollout-1", plan, "app:v2", "app:v1", now);
/// log.append(None, &mut record).unwrap();
/// record.transition(RolloutState::Step(0), now).unwrap();
/// log.append(Some(&RolloutState::Pending), &mut record).unwrap();
///
/// let reopened = RolloutLog::open(log.path()).unwrap();
/// assert_eq!(reopened.load("rollout-1").unwrap().state, RolloutState::Step(0));
/// assert_eq!(reopened.history("rollout-1").unwrap().len(), 2);
/// assert!(reopened.load("rollout-9").is_err());
/// ```
#[derive(Debug, Clone)]
pub struct RolloutLog {
    path: PathBuf,
}

impl RolloutLog {
    /// Open or create the log at `path`, creating parent directories.
    pub fn open(path: &Path) -> Result<Self, RolloutError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Location of the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append `record` as the result of a transition from `from`, only if
    /// the log still holds the rollout exactly as `record` was read: in
    /// state `from` (`None` meaning the rollout does not exist yet) and at
    /// `record.revision` transitions. Comparing the revision as well as the
    /// state stops a stale writer whose state was re-entered by a halt and
    /// roll-forward. The check and the write happen under an exclusive file
    /// lock, so concurrent writers, in this process or another, cannot both
    /// succeed from the same point; the loser gets
    /// [`RolloutError::Conflict`] and must re-read. On success
    /// `record.revision` advances to the new transition count.
    pub fn append(
        &self,
        from: Option<&RolloutState>,
        record: &mut RolloutRecord,
    ) -> Result<(), RolloutError> {
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        fs4::FileExt::lock(&file)?;
        let mut current = None;
        let mut count = 0u64;
        for existing in BufReader::new(&file).lines() {
            let existing = existing?;
            if existing.trim().is_empty() {
                continue;
            }
            let logged: RolloutTransition = serde_json::from_str(&existing)?;
            if logged.record.id == record.id {
                current = Some(logged.record.state);
                count += 1;
            }
        }
        if current.as_ref() != from || count != record.revision {
            let describe = |s: Option<&RolloutState>, revision: u64| {
                s.map_or_else(
                    || "created".to_string(),
                    |s| format!("{s} at revision {revision}"),
                )
            };
            return Err(RolloutError::Conflict {
                id: record.id.clone(),
                expected: describe(from, record.revision),
                actual: describe(current.as_ref(), count),
            });
        }
        let transition = RolloutTransition {
            at: record.updated_at,
            from: from.cloned(),
            record: record.clone(),
        };
        let mut line = serde_json::to_string(&transition)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        record.revision = count + 1;
        Ok(())
    }

    /// Every transition in the log, in order.
    pub fn transitions(&self) -> Result<Vec<RolloutTransition>, RolloutError> {
        let mut out = Vec::new();
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for line in BufReader::new(File::open(&self.path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let mut transition: RolloutTransition = serde_json::from_str(&line)?;
            let count = counts.entry(transition.record.id.clone()).or_insert(0);
            *count += 1;
            transition.record.revision = *count;
            out.push(transition);
        }
        Ok(out)
    }

    /// The transitions of one rollout, in order.
    pub fn history(&self, id: &str) -> Result<Vec<RolloutTransition>, RolloutError> {
        let history: Vec<_> = self
            .transitions()?
            .into_iter()
            .filter(|t| t.record.id == id)
            .collect();
        if history.is_empty() {
            return Err(RolloutError::UnknownRollout(id.to_string()));
        }
        Ok(history)
    }

    /// The current record of every rollout, by id.
    pub fn latest(&self) -> Result<BTreeMap<String, RolloutRecord>, RolloutError> {
        let mut latest = BTreeMap::new();
        for transition in self.transitions()? {
            latest.insert(transition.record.id.clone(), transition.record);
        }
        Ok(latest)
    }

    /// The current record of rollout `id`.
    pub fn load(&self, id: &str) -> Result<RolloutRecord, RolloutError> {
        self.latest()?
            .remove(id)
            .ok_or_else(|| RolloutError::UnknownRollout(id.to_string()))
    }

    /// Kill switch: record rollout `id` as `Halted` at `now`, holding its
    /// traffic split. Needs no adapter, so the CLI can do it from any
    /// process; a running executor notices at its next poll.
    pub fn halt(&self, id: &str, now: DateTime<Utc>) -> Result<RolloutRecord, RolloutError> {
        loop {
            let mut record = self.load(id)?;
            let from = record.state.clone();
            record.transition(RolloutState::Halted, now)?;
            match self.append(Some(&from), &mut record) {
                Err(RolloutError::Conflict { .. }) => continue,
                other => other?,
            }
            tracing::warn!(
                rollout = id,
                traffic = record.traffic_percent,
                "Rollout halted by operator"
            );
            return Ok(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollout::state::tests::{record, t0};

    #[test]
    fn open_creates_parents_and_replays_latest_per_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("rollouts.jsonl");
        let log = RolloutLog::open(&path).unwrap();
        assert_eq!(log.path(), path);
        assert!(log.latest().unwrap().is_empty());
        let mut a = record("production");
        let mut b = record("sandbox");
        b.id = "rollout-2".into();
        log.append(None, &mut a).unwrap();
        log.append(None, &mut b).unwrap();
        a.transition(RolloutState::Step(0), t0()).unwrap();
        log.append(Some(&RolloutState::Pending), &mut a).unwrap();
        let reopened = RolloutLog::open(&path).unwrap();
        let latest = reopened.latest().unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest["rollout-1"].state, RolloutState::Step(0));
        assert_eq!(latest["rollout-2"].state, RolloutState::Pending);
        let history = reopened.history("rollout-1").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].from, None);
        assert_eq!(history[1].from, Some(RolloutState::Pending));
        assert_eq!(history[1].at, t0());
        assert!(
            matches!(reopened.history("rollout-3").unwrap_err(), RolloutError::UnknownRollout(id) if id == "rollout-3")
        );
        assert_eq!(
            reopened.load("rollout-3").unwrap_err().to_string(),
            "unknown rollout `rollout-3`"
        );
    }

    #[test]
    fn halt_holds_any_live_rollout_and_refuses_terminal_ones() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let mut r = record("sandbox");
        r.state = RolloutState::Step(1);
        r.traffic_percent = 10;
        log.append(None, &mut r).unwrap();
        let later = t0() + chrono::Duration::minutes(5);
        let halted = log.halt("rollout-1", later).unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert_eq!(halted.updated_at, later);
        let history = log.history("rollout-1").unwrap();
        assert_eq!(
            history[1].summary(),
            format!("{later}  step 1 -> halted  traffic 10%")
        );
        assert_eq!(
            history[0].summary(),
            format!("{}  created -> step 1  traffic 10%", t0())
        );
        assert!(matches!(
            log.halt("rollout-1", later).unwrap_err(),
            RolloutError::InvalidTransition { .. }
        ));
        assert!(matches!(
            log.halt("rollout-2", later).unwrap_err(),
            RolloutError::UnknownRollout(_)
        ));
    }

    #[test]
    fn append_from_a_stale_state_conflicts_and_leaves_the_log_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let mut r = record("sandbox");
        log.append(None, &mut r).unwrap();
        log.halt("rollout-1", t0()).unwrap();
        r.transition(RolloutState::Step(0), t0()).unwrap();
        let err = log
            .append(Some(&RolloutState::Pending), &mut r)
            .unwrap_err();
        assert!(
            matches!(&err, RolloutError::Conflict { id, .. } if id == "rollout-1"),
            "{err}"
        );
        assert_eq!(log.load("rollout-1").unwrap().state, RolloutState::Halted);
        assert_eq!(log.history("rollout-1").unwrap().len(), 2);
    }

    #[test]
    fn a_stale_append_conflicts_even_when_the_state_matches_again_after_halt_and_roll_forward() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let mut r = record("sandbox");
        log.append(None, &mut r).unwrap();
        r.transition(RolloutState::Step(0), t0()).unwrap();
        log.append(Some(&RolloutState::Pending), &mut r).unwrap();
        let mut stale = log.load("rollout-1").unwrap();
        let halted = log.halt("rollout-1", t0()).unwrap();
        let mut forward = halted.clone();
        forward.transition(RolloutState::Step(0), t0()).unwrap();
        forward.image = "registry.example.invalid/ns/app:v3".into();
        log.append(Some(&RolloutState::Halted), &mut forward)
            .unwrap();
        assert_eq!(log.load("rollout-1").unwrap().state, stale.state);
        stale
            .transition(
                RolloutState::Baking {
                    step: 0,
                    since: t0(),
                },
                t0(),
            )
            .unwrap();
        let err = log
            .append(Some(&RolloutState::Step(0)), &mut stale)
            .unwrap_err();
        assert!(matches!(err, RolloutError::Conflict { .. }), "{err}");
        assert_eq!(log.history("rollout-1").unwrap().len(), 4);
        assert_eq!(
            log.load("rollout-1").unwrap().image,
            "registry.example.invalid/ns/app:v3"
        );
    }

    #[test]
    fn creating_an_existing_id_or_updating_a_missing_one_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let mut r = record("sandbox");
        assert!(matches!(
            log.append(Some(&RolloutState::Pending), &mut r)
                .unwrap_err(),
            RolloutError::Conflict { .. }
        ));
        log.append(None, &mut r).unwrap();
        assert!(matches!(
            log.append(None, &mut r).unwrap_err(),
            RolloutError::Conflict { .. }
        ));
    }

    #[test]
    fn concurrent_appends_from_one_state_admit_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        log.append(None, &mut record("sandbox")).unwrap();
        let outcomes: Vec<_> = (0..8)
            .map(|_| {
                let log = RolloutLog::open(log.path()).unwrap();
                std::thread::spawn(move || {
                    let mut next = log.load("rollout-1").unwrap();
                    next.transition(RolloutState::Step(0), t0()).unwrap();
                    log.append(Some(&RolloutState::Pending), &mut next)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|o| o.is_ok()).count(), 1);
        assert_eq!(log.history("rollout-1").unwrap().len(), 2);
    }

    proptest::proptest! {
        #[test]
        fn append_succeeds_iff_from_matches_the_logged_state(
            logged in 0usize..4,
            claimed in 0usize..4,
        ) {
            let states = [
                RolloutState::Pending,
                RolloutState::Step(0),
                RolloutState::Step(1),
                RolloutState::Halted,
            ];
            let dir = tempfile::tempdir().unwrap();
            let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
            let mut r = record("sandbox");
            r.state = states[logged].clone();
            log.append(None, &mut r).unwrap();
            let outcome = log.append(Some(&states[claimed]), &mut r);
            proptest::prop_assert_eq!(outcome.is_ok(), logged == claimed);
            proptest::prop_assert_eq!(
                log.history("rollout-1").unwrap().len(),
                if logged == claimed { 2 } else { 1 }
            );
        }
    }

    #[test]
    fn blank_lines_are_skipped_and_garbage_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollouts.jsonl");
        let log = RolloutLog::open(&path).unwrap();
        log.append(None, &mut record("sandbox")).unwrap();
        fs::write(&path, format!("{}\n\n", fs::read_to_string(&path).unwrap())).unwrap();
        assert_eq!(log.transitions().unwrap().len(), 1);
        fs::write(&path, "{not json\n").unwrap();
        assert!(matches!(
            log.transitions().unwrap_err(),
            RolloutError::Serde(_)
        ));
    }

    #[test]
    fn path_resolution_prefers_the_override_then_the_queue_log() {
        assert_eq!(
            rollout_path_from(Some("/var/lib/nanna/r.jsonl".into()), None),
            Some(PathBuf::from("/var/lib/nanna/r.jsonl"))
        );
        assert_eq!(
            rollout_path_from(
                None,
                Some(PathBuf::from("/home/u/.local/state/nanna/queue.jsonl"))
            ),
            Some(PathBuf::from("/home/u/.local/state/nanna/rollouts.jsonl"))
        );
        assert_eq!(rollout_path_from(None, None), None);
        let resolved = default_rollout_path();
        let expected = rollout_path_from(std::env::var_os(ROLLOUT_PATH_ENV), default_queue_path());
        assert_eq!(resolved, expected);
    }

    #[test]
    fn io_failures_surface() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "").unwrap();
        let err = RolloutLog::open(&dir.path().join("a").join("b")).unwrap_err();
        assert!(matches!(err, RolloutError::Io(_)), "{err}");
        let log = RolloutLog::open(&dir.path().join("log.jsonl")).unwrap();
        fs::remove_file(log.path()).unwrap();
        assert!(matches!(
            log.append(None, &mut record("sandbox")).unwrap_err(),
            RolloutError::Io(_)
        ));
        assert!(matches!(
            log.transitions().unwrap_err(),
            RolloutError::Io(_)
        ));
    }
}
