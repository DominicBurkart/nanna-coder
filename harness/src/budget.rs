use crate::effects::EffectClass;
use crate::escalation::{Escalation, EscalationSource, Escalator, Severity};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetClass {
    Ci,
    Sandbox,
}

impl BudgetClass {
    pub const ALL: [BudgetClass; 2] = [BudgetClass::Ci, BudgetClass::Sandbox];

    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetClass::Ci => "ci",
            BudgetClass::Sandbox => "sandbox",
        }
    }

    #[doc = r#"The budget class an effect class is charged under; only `Ci` and `Sandbox` are budgeted.

```
use harness::budget::BudgetClass;
use harness::effects::EffectClass;

assert_eq!(BudgetClass::from_effect(EffectClass::Ci), Some(BudgetClass::Ci));
assert_eq!(BudgetClass::from_effect(EffectClass::Production), None);
```"#]
    pub const fn from_effect(class: EffectClass) -> Option<Self> {
        match class {
            EffectClass::Ci => Some(BudgetClass::Ci),
            EffectClass::Sandbox => Some(BudgetClass::Sandbox),
            _ => None,
        }
    }
}

impl fmt::Display for BudgetClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    Task,
    Day,
    Repo,
}

impl fmt::Display for BudgetScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BudgetScope::Task => "per-task",
            BudgetScope::Day => "per-day",
            BudgetScope::Repo => "per-repo",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    Count,
    Minutes,
}

impl fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BudgetDimension::Count => "count",
            BudgetDimension::Minutes => "minutes",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub count: u32,
    pub minutes: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub max_count_per_task: Option<u32>,
    pub max_count_per_day: Option<u32>,
    pub max_minutes_per_task: Option<f64>,
    pub max_minutes_per_day: Option<f64>,
}

impl BudgetLimits {
    pub const DENY_ALL: Self = Self {
        max_count_per_task: Some(0),
        max_count_per_day: Some(0),
        max_minutes_per_task: Some(0.0),
        max_minutes_per_day: Some(0.0),
    };

    pub const UNLIMITED: Self = Self {
        max_count_per_task: None,
        max_count_per_day: None,
        max_minutes_per_task: None,
        max_minutes_per_day: None,
    };

    #[doc = r#"The stricter of each dimension of `self` and `other`; an unset side defers to the other.

```
use harness::budget::BudgetLimits;

let host = BudgetLimits { max_count_per_task: Some(20), ..BudgetLimits::UNLIMITED };
let identity = BudgetLimits { max_count_per_task: Some(3), ..BudgetLimits::UNLIMITED };
assert_eq!(host.narrowed(&identity).max_count_per_task, Some(3));
assert_eq!(identity.narrowed(&host).max_count_per_task, Some(3));
```
"#]
    pub fn narrowed(&self, other: &BudgetLimits) -> BudgetLimits {
        fn stricter<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(if b < a { b } else { a }),
                (a, None) => a,
                (None, b) => b,
            }
        }
        BudgetLimits {
            max_count_per_task: stricter(self.max_count_per_task, other.max_count_per_task),
            max_count_per_day: stricter(self.max_count_per_day, other.max_count_per_day),
            max_minutes_per_task: stricter(self.max_minutes_per_task, other.max_minutes_per_task),
            max_minutes_per_day: stricter(self.max_minutes_per_day, other.max_minutes_per_day),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[doc = r#"Per-class limits.

```
use harness::budget::{BudgetClass, BudgetConfig};

let config = BudgetConfig::default();
assert_eq!(config.limits(BudgetClass::Ci).max_count_per_task, Some(20));
assert_eq!(config.limits(BudgetClass::Sandbox).max_count_per_task, Some(3));
assert_eq!(BudgetConfig::DENY_ALL.ci.max_count_per_day, Some(0));
```"#]
pub struct BudgetConfig {
    pub ci: BudgetLimits,
    pub sandbox: BudgetLimits,
}

impl BudgetConfig {
    pub const DENY_ALL: Self = Self {
        ci: BudgetLimits::DENY_ALL,
        sandbox: BudgetLimits::DENY_ALL,
    };

    pub const UNLIMITED: Self = Self {
        ci: BudgetLimits::UNLIMITED,
        sandbox: BudgetLimits::UNLIMITED,
    };

    pub const fn limits(&self, class: BudgetClass) -> BudgetLimits {
        match class {
            BudgetClass::Ci => self.ci,
            BudgetClass::Sandbox => self.sandbox,
        }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            ci: BudgetLimits {
                max_count_per_task: Some(20),
                max_count_per_day: Some(100),
                max_minutes_per_task: Some(300.0),
                max_minutes_per_day: Some(600.0),
            },
            sandbox: BudgetLimits {
                max_count_per_task: Some(3),
                max_count_per_day: Some(10),
                max_minutes_per_task: Some(120.0),
                max_minutes_per_day: Some(240.0),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
#[error("identity `{identity}` exceeded its {scope} {class} {dimension} budget: {used} > {limit}")]
pub struct BudgetExceeded {
    pub identity: String,
    pub class: BudgetClass,
    pub scope: BudgetScope,
    pub dimension: BudgetDimension,
    pub used: f64,
    pub limit: f64,
}

impl BudgetExceeded {
    fn new(
        identity: &str,
        class: BudgetClass,
        scope: BudgetScope,
        dimension: BudgetDimension,
        used: f64,
        limit: f64,
    ) -> Self {
        Self {
            identity: identity.to_string(),
            class,
            scope,
            dimension,
            used,
            limit,
        }
    }

    pub fn stable_summary(&self) -> String {
        format!(
            "identity `{}` exceeded its {} {} {} budget",
            self.identity, self.scope, self.class, self.dimension
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetReport {
    pub ci: Usage,
    pub sandbox: Usage,
}

impl BudgetReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ci": { "count": self.ci.count, "minutes": self.ci.minutes },
            "sandbox": { "count": self.sandbox.count, "minutes": self.sandbox.minutes },
        })
    }

    pub fn record(&self, telemetry: &crate::telemetry::TelemetrySystem) {
        for (class, usage) in [
            (BudgetClass::Ci, self.ci),
            (BudgetClass::Sandbox, self.sandbox),
        ] {
            telemetry.record_gauge(
                "nanna_budget_count",
                f64::from(usage.count),
                vec![("class", class.as_str())],
            );
            telemetry.record_gauge(
                "nanna_budget_minutes",
                usage.minutes,
                vec![("class", class.as_str())],
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ChargeKey<'a> {
    pub identity: &'a str,
    pub task_id: &'a str,
    pub repo: &'a str,
    pub class: BudgetClass,
    pub day: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum BudgetError {
    #[error(transparent)]
    Exceeded(BudgetExceeded),
    #[error("minutes must be finite and not negative, got {0}")]
    InvalidMinutes(f64),
    #[error("budget ledger could not be persisted to {path}: {message}")]
    Persist { path: PathBuf, message: String },
}

impl BudgetError {
    pub fn exceeded(&self) -> Option<&BudgetExceeded> {
        match self {
            BudgetError::Exceeded(exceeded) => Some(exceeded),
            _ => None,
        }
    }
}

pub trait BudgetStore: Send + Sync {
    fn task_usage(&self, identity: &str, task_id: &str, class: BudgetClass) -> Usage;
    fn day_usage(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage;
    fn repo_day_usage(&self, repo: &str, class: BudgetClass, day: NaiveDate) -> Usage;
    fn record(&self, key: &ChargeKey<'_>, delta: Usage) -> Result<(), BudgetError>;
    fn try_charge(&self, key: &ChargeKey<'_>, limits: &BudgetLimits) -> Result<(), BudgetError>;
    fn note_run(&self, repo: &str, run_id: u64) -> Result<(), BudgetError>;
    fn pending_runs(&self, repo: &str) -> Vec<u64>;
    fn claim_run(&self, repo: &str, run_id: u64) -> Result<bool, BudgetError>;
}

#[derive(Debug, Default, Clone)]
struct Ledger {
    tasks: HashMap<(String, String, BudgetClass), Usage>,
    days: HashMap<(String, BudgetClass, NaiveDate), Usage>,
    repos: HashMap<(String, BudgetClass, NaiveDate), Usage>,
    charged_runs: HashSet<(String, u64)>,
    pending_runs: BTreeSet<(String, u64)>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerSnapshot {
    tasks: Vec<(String, String, BudgetClass, Usage)>,
    days: Vec<(String, BudgetClass, NaiveDate, Usage)>,
    repos: Vec<(String, BudgetClass, NaiveDate, Usage)>,
    charged_runs: Vec<(String, u64)>,
    pending_runs: Vec<(String, u64)>,
}

impl LedgerSnapshot {
    fn validate(&self) -> Result<(), String> {
        let usages = self
            .tasks
            .iter()
            .map(|(_, _, _, usage)| usage)
            .chain(self.days.iter().map(|(_, _, _, usage)| usage))
            .chain(self.repos.iter().map(|(_, _, _, usage)| usage));
        for usage in usages {
            if !usage.minutes.is_finite() || usage.minutes < 0.0 {
                return Err(format!(
                    "minutes must be finite and not negative, got {}",
                    usage.minutes
                ));
            }
        }
        Ok(())
    }
}

fn add(usage: &mut Usage, delta: Usage) {
    usage.count = usage.count.saturating_add(delta.count);
    usage.minutes = (usage.minutes + delta.minutes).min(f64::MAX);
}

impl Ledger {
    fn task(&self, identity: &str, task_id: &str, class: BudgetClass) -> Usage {
        self.tasks
            .get(&(identity.to_string(), task_id.to_string(), class))
            .copied()
            .unwrap_or_default()
    }

    fn day(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.days
            .get(&(identity.to_string(), class, day))
            .copied()
            .unwrap_or_default()
    }

    fn repo_day(&self, repo: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.repos
            .get(&(repo.to_string(), class, day))
            .copied()
            .unwrap_or_default()
    }

    fn record(&mut self, key: &ChargeKey<'_>, delta: Usage) -> Result<(), BudgetError> {
        if !delta.minutes.is_finite() || delta.minutes < 0.0 {
            return Err(BudgetError::InvalidMinutes(delta.minutes));
        }
        add(
            self.tasks
                .entry((key.identity.to_string(), key.task_id.to_string(), key.class))
                .or_default(),
            delta,
        );
        add(
            self.days
                .entry((key.identity.to_string(), key.class, key.day))
                .or_default(),
            delta,
        );
        add(
            self.repos
                .entry((key.repo.to_string(), key.class, key.day))
                .or_default(),
            delta,
        );
        Ok(())
    }

    fn try_charge(
        &mut self,
        key: &ChargeKey<'_>,
        limits: &BudgetLimits,
    ) -> Result<(), BudgetError> {
        let scopes = [
            (
                BudgetScope::Task,
                self.task(key.identity, key.task_id, key.class),
                limits.max_count_per_task,
                limits.max_minutes_per_task,
            ),
            (
                BudgetScope::Day,
                self.day(key.identity, key.class, key.day),
                limits.max_count_per_day,
                limits.max_minutes_per_day,
            ),
            (
                BudgetScope::Repo,
                self.repo_day(key.repo, key.class, key.day),
                limits.max_count_per_day,
                limits.max_minutes_per_day,
            ),
        ];
        for (scope, usage, max_count, max_minutes) in scopes {
            if let Some(max) = max_count {
                if usage.count >= max {
                    return Err(BudgetError::Exceeded(BudgetExceeded::new(
                        key.identity,
                        key.class,
                        scope,
                        BudgetDimension::Count,
                        f64::from(usage.count),
                        f64::from(max),
                    )));
                }
            }
            if let Some(max) = max_minutes {
                if usage.minutes >= max {
                    return Err(BudgetError::Exceeded(BudgetExceeded::new(
                        key.identity,
                        key.class,
                        scope,
                        BudgetDimension::Minutes,
                        usage.minutes,
                        max,
                    )));
                }
            }
        }
        self.record(
            key,
            Usage {
                count: 1,
                minutes: 0.0,
            },
        )
    }

    fn note_run(&mut self, repo: &str, run_id: u64) -> bool {
        let id = (repo.to_string(), run_id);
        if self.charged_runs.contains(&id) {
            return false;
        }
        self.pending_runs.insert(id)
    }

    fn pending(&self, repo: &str) -> Vec<u64> {
        self.pending_runs
            .iter()
            .filter(|(r, _)| r == repo)
            .map(|(_, id)| *id)
            .collect()
    }

    fn claim_run(&mut self, repo: &str, run_id: u64) -> bool {
        let id = (repo.to_string(), run_id);
        self.pending_runs.remove(&id);
        self.charged_runs.insert(id)
    }

    fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            tasks: self
                .tasks
                .iter()
                .map(|((i, t, c), u)| (i.clone(), t.clone(), *c, *u))
                .collect(),
            days: self
                .days
                .iter()
                .map(|((i, c, d), u)| (i.clone(), *c, *d, *u))
                .collect(),
            repos: self
                .repos
                .iter()
                .map(|((r, c, d), u)| (r.clone(), *c, *d, *u))
                .collect(),
            charged_runs: self.charged_runs.iter().cloned().collect(),
            pending_runs: self.pending_runs.iter().cloned().collect(),
        }
    }

    fn from_snapshot(snapshot: LedgerSnapshot) -> Self {
        Self {
            tasks: snapshot
                .tasks
                .into_iter()
                .map(|(i, t, c, u)| ((i, t, c), u))
                .collect(),
            days: snapshot
                .days
                .into_iter()
                .map(|(i, c, d, u)| ((i, c, d), u))
                .collect(),
            repos: snapshot
                .repos
                .into_iter()
                .map(|(r, c, d, u)| ((r, c, d), u))
                .collect(),
            charged_runs: snapshot.charged_runs.into_iter().collect(),
            pending_runs: snapshot.pending_runs.into_iter().collect(),
        }
    }
}

#[derive(Debug, Default)]
pub struct InMemoryBudgetStore {
    ledger: Mutex<Ledger>,
}

impl InMemoryBudgetStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.ledger.lock().expect("budget ledger poisoned")
    }
}

impl BudgetStore for InMemoryBudgetStore {
    fn task_usage(&self, identity: &str, task_id: &str, class: BudgetClass) -> Usage {
        self.ledger().task(identity, task_id, class)
    }

    fn day_usage(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.ledger().day(identity, class, day)
    }

    fn repo_day_usage(&self, repo: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.ledger().repo_day(repo, class, day)
    }

    fn record(&self, key: &ChargeKey<'_>, delta: Usage) -> Result<(), BudgetError> {
        self.ledger().record(key, delta)
    }

    fn try_charge(&self, key: &ChargeKey<'_>, limits: &BudgetLimits) -> Result<(), BudgetError> {
        self.ledger().try_charge(key, limits)
    }

    fn note_run(&self, repo: &str, run_id: u64) -> Result<(), BudgetError> {
        self.ledger().note_run(repo, run_id);
        Ok(())
    }

    fn pending_runs(&self, repo: &str) -> Vec<u64> {
        self.ledger().pending(repo)
    }

    fn claim_run(&self, repo: &str, run_id: u64) -> Result<bool, BudgetError> {
        Ok(self.ledger().claim_run(repo, run_id))
    }
}

#[derive(Debug)]
pub struct FileBudgetStore {
    path: PathBuf,
    ledger: Mutex<Ledger>,
    _lock: std::fs::File,
}

pub const BUDGET_PATH_ENV: &str = "NANNA_BUDGET_PATH";

pub fn budget_path_from(
    override_path: Option<std::ffi::OsString>,
    queue_path: Option<PathBuf>,
) -> Option<PathBuf> {
    override_path
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| queue_path.map(|q| q.with_file_name("budget.json")))
}

#[derive(Debug, Error)]
pub enum BudgetStoreError {
    #[error("budget store {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("budget store {path} is in use by another process")]
    Locked { path: PathBuf },
    #[error("budget store {path} holds an invalid value: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("budget store {path} is corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
}

fn lock_outcome(
    path: &std::path::Path,
    lock_path: &std::path::Path,
    attempt: Result<(), fs4::TryLockError>,
) -> Result<(), BudgetStoreError> {
    match attempt {
        Ok(()) => Ok(()),
        Err(fs4::TryLockError::WouldBlock) => Err(BudgetStoreError::Locked {
            path: path.to_path_buf(),
        }),
        Err(fs4::TryLockError::Error(source)) => Err(BudgetStoreError::Io {
            path: lock_path.to_path_buf(),
            source,
        }),
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

impl FileBudgetStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, BudgetStoreError> {
        let path = path.into();
        let lock = Self::acquire_lock(&path)?;
        let ledger = match std::fs::read(&path) {
            Ok(bytes) => {
                let snapshot: LedgerSnapshot =
                    serde_json::from_slice(&bytes).map_err(|source| BudgetStoreError::Corrupt {
                        path: path.clone(),
                        source,
                    })?;
                snapshot
                    .validate()
                    .map_err(|reason| BudgetStoreError::Invalid {
                        path: path.clone(),
                        reason,
                    })?;
                Ledger::from_snapshot(snapshot)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
            Err(source) => return Err(BudgetStoreError::Io { path, source }),
        };
        Ok(Self {
            path,
            ledger: Mutex::new(ledger),
            _lock: lock,
        })
    }

    fn acquire_lock(path: &std::path::Path) -> Result<std::fs::File, BudgetStoreError> {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let lock_path = PathBuf::from(name);
        let io = |source| BudgetStoreError::Io {
            path: lock_path.clone(),
            source,
        };
        if let Some(parent) = lock_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io)?;
        lock_outcome(path, &lock_path, fs4::FileExt::try_lock(&file)).map(|()| file)
    }

    fn persist(&self, ledger: &Ledger) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(&ledger.snapshot())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        write_private(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)
    }

    fn mutate<R>(
        &self,
        f: impl FnOnce(&mut Ledger) -> (Result<R, BudgetError>, bool),
    ) -> Result<R, BudgetError> {
        let mut ledger = self.ledger.lock().expect("budget ledger poisoned");
        let before = ledger.clone();
        let (result, changed) = f(&mut ledger);
        if changed {
            if let Err(e) = self.persist(&ledger) {
                *ledger = before;
                return Err(BudgetError::Persist {
                    path: self.path.clone(),
                    message: e.to_string(),
                });
            }
        }
        result
    }

    fn read<R>(&self, f: impl FnOnce(&Ledger) -> R) -> R {
        f(&self.ledger.lock().expect("budget ledger poisoned"))
    }
}

impl BudgetStore for FileBudgetStore {
    fn task_usage(&self, identity: &str, task_id: &str, class: BudgetClass) -> Usage {
        self.read(|l| l.task(identity, task_id, class))
    }

    fn day_usage(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.read(|l| l.day(identity, class, day))
    }

    fn repo_day_usage(&self, repo: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.read(|l| l.repo_day(repo, class, day))
    }

    fn record(&self, key: &ChargeKey<'_>, delta: Usage) -> Result<(), BudgetError> {
        self.mutate(|l| {
            let result = l.record(key, delta);
            let changed = result.is_ok();
            (result, changed)
        })
    }

    fn try_charge(&self, key: &ChargeKey<'_>, limits: &BudgetLimits) -> Result<(), BudgetError> {
        self.mutate(|l| {
            let result = l.try_charge(key, limits);
            let changed = result.is_ok();
            (result, changed)
        })
    }

    fn note_run(&self, repo: &str, run_id: u64) -> Result<(), BudgetError> {
        self.mutate(|l| {
            let changed = l.note_run(repo, run_id);
            (Ok(()), changed)
        })
    }

    fn pending_runs(&self, repo: &str) -> Vec<u64> {
        self.read(|l| l.pending(repo))
    }

    fn claim_run(&self, repo: &str, run_id: u64) -> Result<bool, BudgetError> {
        self.mutate(|l| {
            let first = l.claim_run(repo, run_id);
            (Ok(first), true)
        })
    }
}

#[doc = r#"A per-identity ceiling on `Ci` and `Sandbox` actions.

```
use harness::budget::{BudgetClass, CostAccountant, InMemoryBudgetStore};
use std::sync::Arc;

let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
runtime.block_on(async {
    let accountant = CostAccountant::deny_all(Arc::new(InMemoryBudgetStore::new()));
    let denied = accountant
        .charge_count("id", "t1", "o/n", BudgetClass::Ci, chrono::Utc::now())
        .await
        .unwrap_err();
    let denied = denied.exceeded().unwrap();
    assert_eq!(
        denied.stable_summary(),
        "identity `id` exceeded its per-task ci count budget"
    );
    assert_eq!(accountant.task_summary("id", "t1").ci.count, 0);
});
```"#]
pub struct CostAccountant {
    store: Arc<dyn BudgetStore>,
    config: BudgetConfig,
    escalator: Option<Arc<Escalator>>,
}

impl CostAccountant {
    pub fn deny_all(store: Arc<dyn BudgetStore>) -> Self {
        Self::new(store, BudgetConfig::DENY_ALL)
    }

    pub fn new(store: Arc<dyn BudgetStore>, config: BudgetConfig) -> Self {
        Self {
            store,
            config,
            escalator: None,
        }
    }

    pub fn with_escalator(mut self, escalator: Arc<Escalator>) -> Self {
        self.escalator = Some(escalator);
        self
    }

    pub fn task_summary(&self, identity: &str, task_id: &str) -> BudgetReport {
        BudgetReport {
            ci: self.store.task_usage(identity, task_id, BudgetClass::Ci),
            sandbox: self
                .store
                .task_usage(identity, task_id, BudgetClass::Sandbox),
        }
    }

    async fn deny(&self, repo: &str, exceeded: BudgetExceeded) -> BudgetExceeded {
        if let Some(escalator) = &self.escalator {
            let escalation = Escalation::new(
                Severity::Blocked,
                EscalationSource::Budget,
                repo,
                exceeded.stable_summary(),
            )
            .with_identity(&exceeded.identity)
            .with_evidence(vec![exceeded.to_string()])
            .with_suggested_action(
                "Raise this identity's budget, or wait for the day/task window to reset.",
            );
            if let Err(e) = escalator.escalate(escalation).await {
                tracing::warn!("budget escalation delivery failed: {e}");
            }
        }
        exceeded
    }

    pub async fn charge_count(
        &self,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        now: DateTime<Utc>,
    ) -> Result<BudgetReport, BudgetError> {
        self.charge_count_within(
            &BudgetLimits::UNLIMITED,
            identity,
            task_id,
            repo,
            class,
            now,
        )
        .await
    }

    pub async fn charge_count_within(
        &self,
        ceiling: &BudgetLimits,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        now: DateTime<Utc>,
    ) -> Result<BudgetReport, BudgetError> {
        let limits = self.config.limits(class).narrowed(ceiling);
        let key = ChargeKey {
            identity,
            task_id,
            repo,
            class,
            day: now.date_naive(),
        };
        match self.store.try_charge(&key, &limits) {
            Ok(()) => Ok(self.task_summary(identity, task_id)),
            Err(BudgetError::Exceeded(exceeded)) => {
                Err(BudgetError::Exceeded(self.deny(repo, exceeded).await))
            }
            Err(other) => Err(other),
        }
    }

    pub fn record_minutes_sync(
        &self,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        minutes: f64,
        now: DateTime<Utc>,
    ) -> Result<BudgetReport, BudgetError> {
        let key = ChargeKey {
            identity,
            task_id,
            repo,
            class,
            day: now.date_naive(),
        };
        self.store.record(&key, Usage { count: 0, minutes })?;
        Ok(self.task_summary(identity, task_id))
    }

    pub async fn record_minutes(
        &self,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        minutes: f64,
        now: DateTime<Utc>,
    ) -> Result<BudgetReport, BudgetError> {
        let day = now.date_naive();
        let key = ChargeKey {
            identity,
            task_id,
            repo,
            class,
            day,
        };
        self.store.record(&key, Usage { count: 0, minutes })?;
        let limits = self.config.limits(class);
        let observed = [
            (
                BudgetScope::Task,
                self.store.task_usage(identity, task_id, class),
            ),
            (BudgetScope::Day, self.store.day_usage(identity, class, day)),
            (
                BudgetScope::Repo,
                self.store.repo_day_usage(repo, class, day),
            ),
        ];
        for (scope, usage) in observed {
            let max = match scope {
                BudgetScope::Task => limits.max_minutes_per_task,
                BudgetScope::Day | BudgetScope::Repo => limits.max_minutes_per_day,
            };
            if let Some(max) = max {
                if usage.minutes > max {
                    let exceeded = BudgetExceeded::new(
                        identity,
                        class,
                        scope,
                        BudgetDimension::Minutes,
                        usage.minutes,
                        max,
                    );
                    self.deny(repo, exceeded).await;
                }
            }
        }
        Ok(self.task_summary(identity, task_id))
    }

    pub fn note_run(&self, repo: &str, run_id: u64) -> Result<(), BudgetError> {
        self.store.note_run(repo, run_id)
    }

    pub fn pending_runs(&self, repo: &str) -> Vec<u64> {
        self.store.pending_runs(repo)
    }

    pub fn claim_run(&self, repo: &str, run_id: u64) -> Result<bool, BudgetError> {
        self.store.claim_run(repo, run_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::{
        DeliveryReceipt, EscalationError, EscalationLog, EscalationSink, FanoutSink,
    };
    use crate::leases::SystemClock;
    use chrono::{TimeZone, Utc};

    struct FailingSink;

    #[async_trait::async_trait]
    impl EscalationSink for FailingSink {
        fn name(&self) -> &str {
            "failing"
        }

        async fn deliver(
            &self,
            _escalation: &Escalation,
        ) -> Result<DeliveryReceipt, EscalationError> {
            Err(EscalationError::Status {
                url: "https://example.invalid".to_string(),
                status: 500,
            })
        }
    }

    fn reopen(path: &std::path::Path) -> FileBudgetStore {
        for _ in 0..500 {
            match FileBudgetStore::open(path) {
                Err(BudgetStoreError::Locked { .. }) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                other => return other.unwrap(),
            }
        }
        FileBudgetStore::open(path).unwrap()
    }

    fn accountant(config: BudgetConfig) -> CostAccountant {
        CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap()
    }

    #[test]
    fn budget_class_maps_from_effect_class_and_only_two_classes_map() {
        assert_eq!(
            BudgetClass::from_effect(EffectClass::Ci),
            Some(BudgetClass::Ci)
        );
        assert_eq!(
            BudgetClass::from_effect(EffectClass::Sandbox),
            Some(BudgetClass::Sandbox)
        );
        for other in [
            EffectClass::None,
            EffectClass::Workspace,
            EffectClass::Repository,
            EffectClass::Production,
        ] {
            assert_eq!(BudgetClass::from_effect(other), None, "{other}");
        }
    }

    #[test]
    fn display_impls_are_human_readable() {
        assert_eq!(BudgetClass::Ci.to_string(), "ci");
        assert_eq!(BudgetClass::Sandbox.to_string(), "sandbox");
        assert_eq!(BudgetScope::Task.to_string(), "per-task");
        assert_eq!(BudgetScope::Day.to_string(), "per-day");
        assert_eq!(BudgetDimension::Count.to_string(), "count");
        assert_eq!(BudgetDimension::Minutes.to_string(), "minutes");
    }

    #[test]
    fn config_limits_selects_the_right_class() {
        let config = BudgetConfig::default();
        assert_eq!(config.limits(BudgetClass::Ci), config.ci);
        assert_eq!(config.limits(BudgetClass::Sandbox), config.sandbox);
        assert_eq!(
            BudgetConfig::UNLIMITED
                .limits(BudgetClass::Ci)
                .max_count_per_task,
            None
        );
    }

    #[tokio::test]
    async fn unlimited_config_never_blocks() {
        let accountant = accountant(BudgetConfig::UNLIMITED);
        for _ in 0..50 {
            accountant
                .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
                .await
                .unwrap();
        }
        let summary = accountant.task_summary("id", "t1");
        assert_eq!(summary.ci.count, 50);
    }

    #[tokio::test]
    async fn per_task_count_ceiling_blocks_once_reached_but_leaves_other_tasks_unaffected() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(2),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Task);
        assert_eq!(err.dimension, BudgetDimension::Count);
        assert_eq!(err.used, 2.0);
        assert_eq!(err.limit, 2.0);
        assert_eq!(
            err.to_string(),
            "identity `id` exceeded its per-task ci count budget: 2 > 2"
        );
        accountant
            .charge_count("id", "t2", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn per_day_count_ceiling_spans_tasks_for_the_same_identity_but_not_others_on_another_repo(
    ) {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_day: Some(1),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t2", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Day);
        assert_eq!(err.dimension, BudgetDimension::Count);
        accountant
            .charge_count("other", "t3", "other/repo", BudgetClass::Ci, now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn per_task_and_per_day_minutes_ceilings_block_the_next_charge() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(10.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 15.0, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.dimension, BudgetDimension::Minutes);
        assert_eq!(err.used, 15.0);
    }

    #[tokio::test]
    async fn per_day_minutes_ceiling_blocks_across_tasks() {
        let config = BudgetConfig {
            ci: BudgetLimits::UNLIMITED,
            sandbox: BudgetLimits {
                max_minutes_per_day: Some(30.0),
                ..BudgetLimits::UNLIMITED
            },
        };
        let accountant = accountant(config);
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Sandbox, now())
            .await
            .unwrap();
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Sandbox, 45.0, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t2", "o/n", BudgetClass::Sandbox, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Day);
        assert_eq!(err.dimension, BudgetDimension::Minutes);
    }

    #[tokio::test]
    async fn record_minutes_never_blocks_the_call_that_observed_them() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(1.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        let summary = accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 500.0, now())
            .await
            .unwrap();
        assert_eq!(summary.ci.minutes, 500.0);
    }

    #[test]
    fn record_minutes_sync_records_without_awaiting_or_checking_limits() {
        let config = BudgetConfig {
            ci: BudgetLimits::UNLIMITED,
            sandbox: BudgetLimits {
                max_minutes_per_task: Some(1.0),
                ..BudgetLimits::UNLIMITED
            },
        };
        let accountant = accountant(config);
        let summary = accountant
            .record_minutes_sync("id", "t1", "o/n", BudgetClass::Sandbox, 50.0, now())
            .unwrap();
        assert_eq!(summary.sandbox.minutes, 50.0);
        assert_eq!(accountant.task_summary("id", "t1").sandbox.minutes, 50.0);
    }

    #[tokio::test]
    async fn task_summary_reports_both_classes_independently() {
        let accountant = accountant(BudgetConfig::UNLIMITED);
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Sandbox, now())
            .await
            .unwrap();
        let summary = accountant.task_summary("id", "t1");
        assert_eq!(summary.ci.count, 1);
        assert_eq!(summary.sandbox.count, 1);
        let empty = accountant.task_summary("id", "unknown");
        assert_eq!(empty, BudgetReport::default());
    }

    #[tokio::test]
    async fn exhaustion_escalates_when_an_escalator_is_attached() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let log = Arc::new(EscalationLog::in_memory());
        let escalator = Arc::new(Escalator::new(
            Arc::clone(&log),
            Arc::new(FanoutSink(vec![])),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        let accountant = CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
            .with_escalator(escalator);
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.dimension, BudgetDimension::Count);
        let snapshot = log.snapshot(Utc::now());
        assert_eq!(snapshot.tracked, 1);
    }

    #[test]
    fn stable_summary_omits_the_changing_used_and_limit_figures() {
        let a = BudgetExceeded::new(
            "id",
            BudgetClass::Ci,
            BudgetScope::Day,
            BudgetDimension::Minutes,
            15.0,
            10.0,
        );
        let b = BudgetExceeded::new(
            "id",
            BudgetClass::Ci,
            BudgetScope::Day,
            BudgetDimension::Minutes,
            25.0,
            10.0,
        );
        assert_eq!(a.stable_summary(), b.stable_summary());
        assert_ne!(a.to_string(), b.to_string());
        assert_eq!(
            a.stable_summary(),
            "identity `id` exceeded its per-day ci minutes budget"
        );
    }

    #[tokio::test]
    async fn repeated_exhaustion_collapses_into_one_escalation_despite_the_growing_minutes() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(1.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let log = Arc::new(EscalationLog::in_memory());
        let escalator = Arc::new(Escalator::new(
            Arc::clone(&log),
            Arc::new(FanoutSink(vec![])),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        let accountant = CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
            .with_escalator(escalator);
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 5.0, now())
            .await
            .unwrap();
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 5.0, now())
            .await
            .unwrap();
        let snapshot = log.snapshot(Utc::now());
        assert_eq!(
            snapshot.tracked, 1,
            "both breaches must dedupe to the same key despite different `used` values"
        );
    }

    #[tokio::test]
    async fn minutes_exhaustion_also_escalates_without_blocking_the_observer() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(1.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let log = Arc::new(EscalationLog::in_memory());
        let escalator = Arc::new(Escalator::new(
            Arc::clone(&log),
            Arc::new(FanoutSink(vec![])),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        let accountant = CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
            .with_escalator(escalator);
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 5.0, now())
            .await
            .unwrap();
        let snapshot = log.snapshot(Utc::now());
        assert_eq!(snapshot.tracked, 1);
    }

    #[tokio::test]
    async fn escalation_delivery_failure_does_not_change_the_returned_reason() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let log = Arc::new(EscalationLog::in_memory());
        let escalator = Arc::new(Escalator::new(
            log,
            Arc::new(FailingSink),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        let accountant = CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
            .with_escalator(escalator);
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.dimension, BudgetDimension::Count);
    }

    struct MetricCapture(Arc<Mutex<Vec<crate::telemetry::MetricPoint>>>);

    #[async_trait::async_trait]
    impl crate::telemetry::TelemetryExporter for MetricCapture {
        async fn export_traces(
            &self,
            _traces: Vec<crate::telemetry::TraceContext>,
        ) -> Result<(), crate::telemetry::TelemetryError> {
            Ok(())
        }

        async fn export_metrics(
            &self,
            metrics: Vec<crate::telemetry::MetricPoint>,
        ) -> Result<(), crate::telemetry::TelemetryError> {
            self.0.lock().unwrap().extend(metrics);
            Ok(())
        }

        async fn export_events(
            &self,
            _events: Vec<crate::telemetry::CustomEvent>,
        ) -> Result<(), crate::telemetry::TelemetryError> {
            Ok(())
        }

        async fn export_system_metrics(
            &self,
            _metrics: crate::monitoring::SystemMetrics,
        ) -> Result<(), crate::telemetry::TelemetryError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<bool, crate::telemetry::TelemetryError> {
            Ok(true)
        }
    }

    fn sample_report() -> BudgetReport {
        BudgetReport {
            ci: Usage {
                count: 3,
                minutes: 12.5,
            },
            sandbox: Usage {
                count: 1,
                minutes: 4.0,
            },
        }
    }

    #[test]
    fn budget_report_json_keeps_each_class_and_dimension_apart() {
        let json = sample_report().to_json();
        assert_eq!(json["ci"]["count"], 3);
        assert_eq!(json["ci"]["minutes"], 12.5);
        assert_eq!(json["sandbox"]["count"], 1);
        assert_eq!(json["sandbox"]["minutes"], 4.0);
    }

    #[tokio::test]
    async fn budget_report_records_one_gauge_per_class_and_dimension() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let telemetry = crate::telemetry::TelemetrySystem::new()
            .add_exporter(Box::new(MetricCapture(Arc::clone(&captured))));
        sample_report().record(&telemetry);
        telemetry.export_all().await.unwrap();
        let mut seen: Vec<(String, String, f64)> = captured
            .lock()
            .unwrap()
            .iter()
            .map(|m| (m.name.clone(), m.labels["class"].clone(), m.value))
            .collect();
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            seen,
            vec![
                ("nanna_budget_count".to_string(), "ci".to_string(), 3.0),
                ("nanna_budget_count".to_string(), "sandbox".to_string(), 1.0),
                ("nanna_budget_minutes".to_string(), "ci".to_string(), 12.5),
                (
                    "nanna_budget_minutes".to_string(),
                    "sandbox".to_string(),
                    4.0
                ),
            ]
        );
    }

    #[test]
    fn in_memory_store_isolates_tasks_days_and_identities() {
        let store = InMemoryBudgetStore::new();
        let day = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .unwrap()
            .date_naive();
        store
            .record(
                &ChargeKey {
                    identity: "id",
                    task_id: "t1",
                    repo: "o/n",
                    class: BudgetClass::Ci,
                    day,
                },
                Usage {
                    count: 1,
                    minutes: 2.0,
                },
            )
            .unwrap();
        assert_eq!(
            store.task_usage("id", "t1", BudgetClass::Ci),
            Usage {
                count: 1,
                minutes: 2.0
            }
        );
        assert_eq!(
            store.task_usage("id", "t1", BudgetClass::Sandbox),
            Usage::default()
        );
        assert_eq!(
            store.task_usage("id", "other", BudgetClass::Ci),
            Usage::default()
        );
        assert_eq!(
            store.day_usage("id", BudgetClass::Ci, day),
            Usage {
                count: 1,
                minutes: 2.0
            }
        );
        assert_eq!(
            store.day_usage("id", BudgetClass::Ci, day.succ_opt().unwrap()),
            Usage::default()
        );
        assert_eq!(
            store.day_usage("other", BudgetClass::Ci, day),
            Usage::default()
        );
    }

    #[test]
    fn budget_limits_unlimited_has_no_ceilings() {
        assert_eq!(BudgetLimits::UNLIMITED.max_count_per_task, None);
        assert_eq!(BudgetLimits::UNLIMITED.max_count_per_day, None);
        assert_eq!(BudgetLimits::UNLIMITED.max_minutes_per_task, None);
        assert_eq!(BudgetLimits::UNLIMITED.max_minutes_per_day, None);
    }

    #[test]
    fn default_config_has_tighter_sandbox_limits_than_ci() {
        let config = BudgetConfig::default();
        assert!(config.sandbox.max_count_per_task < config.ci.max_count_per_task);
        assert!(config.sandbox.max_count_per_day < config.ci.max_count_per_day);
    }

    fn limited_count(per_task: u32, per_day: u32) -> BudgetConfig {
        BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(per_task),
                max_count_per_day: Some(per_day),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        }
    }

    #[test]
    fn concurrent_charges_on_the_store_cannot_overshoot_the_ceiling() {
        let store = Arc::new(InMemoryBudgetStore::new());
        let limits = limited_count(5, 1000).ci;
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let day = now().date_naive();
        let handles: Vec<_> = (0..32)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .try_charge(
                            &ChargeKey {
                                identity: "id",
                                task_id: "t1",
                                repo: "o/n",
                                class: BudgetClass::Ci,
                                day,
                            },
                            &limits,
                        )
                        .is_ok()
                })
            })
            .collect();
        let granted = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(granted, 5);
        assert_eq!(store.task_usage("id", "t1", BudgetClass::Ci).count, 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_accountant_charges_cannot_overshoot_the_ceiling() {
        let accountant = Arc::new(accountant(limited_count(3, 1000)));
        let handles: Vec<_> = (0..24)
            .map(|_| {
                let accountant = Arc::clone(&accountant);
                tokio::spawn(async move {
                    accountant
                        .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
                        .await
                        .is_ok()
                })
            })
            .collect();
        let mut granted = 0;
        for h in handles {
            if h.await.unwrap() {
                granted += 1;
            }
        }
        assert_eq!(granted, 3);
        assert_eq!(accountant.task_summary("id", "t1").ci.count, 3);
    }

    #[tokio::test]
    async fn file_store_budget_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("budget.json");
        {
            let accountant = CostAccountant::new(
                Arc::new(FileBudgetStore::open(&path).unwrap()),
                limited_count(2, 1000),
            );
            accountant
                .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
                .await
                .unwrap();
            accountant
                .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
                .await
                .unwrap();
            accountant.note_run("o/n", 7).unwrap();
        }
        let reopened = CostAccountant::new(
            Arc::new(FileBudgetStore::open(&path).unwrap()),
            limited_count(2, 1000),
        );
        assert_eq!(reopened.task_summary("id", "t1").ci.count, 2);
        let err = reopened
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Task);
        assert_eq!(reopened.pending_runs("o/n"), vec![7]);
        assert!(reopened.claim_run("o/n", 7).unwrap());
        assert!(reopened.pending_runs("o/n").is_empty());
    }

    #[test]
    fn file_store_opens_empty_when_missing_and_rejects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let store = FileBudgetStore::open(&path).unwrap();
        assert_eq!(
            store.task_usage("id", "t", BudgetClass::Ci),
            Usage::default()
        );
        drop(store);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            FileBudgetStore::open(&path),
            Err(BudgetStoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn a_second_open_of_the_same_store_is_refused_while_the_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let first = FileBudgetStore::open(&path).unwrap();
        assert!(matches!(
            FileBudgetStore::open(&path),
            Err(BudgetStoreError::Locked { .. })
        ));
        drop(first);
        assert!(FileBudgetStore::open(&path).is_ok());
    }

    #[test]
    fn stores_at_different_paths_do_not_contend() {
        let dir = tempfile::tempdir().unwrap();
        let a = FileBudgetStore::open(dir.path().join("a.json")).unwrap();
        let b = FileBudgetStore::open(dir.path().join("b.json")).unwrap();
        drop((a, b));
    }

    #[test]
    fn budget_path_prefers_the_override_then_sits_next_to_the_queue_log() {
        use std::ffi::OsString;
        assert_eq!(
            budget_path_from(
                Some(OsString::from("/srv/b.json")),
                Some(PathBuf::from("/q/queue.jsonl"))
            ),
            Some(PathBuf::from("/srv/b.json"))
        );
        assert_eq!(
            budget_path_from(None, Some(PathBuf::from("/q/queue.jsonl"))),
            Some(PathBuf::from("/q/budget.json"))
        );
        assert_eq!(
            budget_path_from(Some(OsString::new()), Some(PathBuf::from("/q/queue.jsonl"))),
            Some(PathBuf::from("/q/budget.json"))
        );
        assert_eq!(budget_path_from(None, None), None);
        assert_eq!(budget_path_from(Some(OsString::new()), None), None);
    }

    #[tokio::test]
    async fn deny_all_refuses_every_charge_for_both_classes() {
        let accountant = CostAccountant::deny_all(Arc::new(InMemoryBudgetStore::new()));
        for class in [BudgetClass::Ci, BudgetClass::Sandbox] {
            assert!(accountant
                .charge_count("id", "t", "o/n", class, now())
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn day_ceiling_is_keyed_per_repo_so_new_identities_and_tasks_cannot_bypass_it() {
        let accountant = accountant(limited_count(1000, 3));
        for n in 0..3 {
            accountant
                .charge_count(
                    &format!("id{n}"),
                    &format!("t{n}"),
                    "o/n",
                    BudgetClass::Ci,
                    now(),
                )
                .await
                .unwrap();
        }
        let err = accountant
            .charge_count("id9", "t9", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Repo);
        accountant
            .charge_count("id9", "t9", "other/repo", BudgetClass::Ci, now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn repo_minutes_accumulated_across_identities_block_further_triggers() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_day: Some(10.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .record_minutes("a", "t1", "o/n", BudgetClass::Ci, 6.0, now())
            .await
            .unwrap();
        accountant
            .record_minutes("b", "t2", "o/n", BudgetClass::Ci, 6.0, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("c", "t3", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Repo);
        assert_eq!(err.dimension, BudgetDimension::Minutes);
    }

    #[test]
    fn a_run_is_claimed_once_and_only_pending_until_then() {
        let accountant = accountant(BudgetConfig::UNLIMITED);
        accountant.note_run("o/n", 1).unwrap();
        accountant.note_run("o/n", 1).unwrap();
        accountant.note_run("x/y", 2).unwrap();
        assert_eq!(accountant.pending_runs("o/n"), vec![1]);
        assert!(accountant.claim_run("o/n", 1).unwrap());
        assert!(!accountant.claim_run("o/n", 1).unwrap());
        accountant.note_run("o/n", 1).unwrap();
        assert!(accountant.pending_runs("o/n").is_empty());
        assert_eq!(accountant.pending_runs("x/y"), vec![2]);
    }

    #[test]
    fn budget_scope_repo_displays_as_per_repo() {
        assert_eq!(BudgetScope::Repo.to_string(), "per-repo");
    }

    struct CapturingSink(Arc<Mutex<Vec<Escalation>>>);

    #[async_trait::async_trait]
    impl EscalationSink for CapturingSink {
        fn name(&self) -> &str {
            "capturing"
        }

        async fn deliver(
            &self,
            escalation: &Escalation,
        ) -> Result<DeliveryReceipt, EscalationError> {
            self.0.lock().unwrap().push(escalation.clone());
            Ok(DeliveryReceipt {
                sink: "capturing".to_string(),
                reference: escalation.id.clone(),
                outcome: crate::escalation::DeliveryOutcome::Posted,
            })
        }
    }

    fn capturing_accountant(config: BudgetConfig) -> (CostAccountant, Arc<Mutex<Vec<Escalation>>>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let escalator = Arc::new(Escalator::new(
            Arc::new(EscalationLog::in_memory()),
            Arc::new(FanoutSink(vec![Box::new(CapturingSink(Arc::clone(&sink)))])),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        (
            CostAccountant::new(Arc::new(InMemoryBudgetStore::new()), config)
                .with_escalator(escalator),
            sink,
        )
    }

    #[tokio::test]
    async fn a_denial_escalates_as_blocked_budget_for_the_repo_and_identity_with_the_figures() {
        let (accountant, sink) = capturing_accountant(BudgetConfig::DENY_ALL);
        accountant
            .charge_count("agent-a", "t1", "o/n", BudgetClass::Sandbox, now())
            .await
            .unwrap_err();
        let seen = sink.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::Blocked);
        assert_eq!(seen[0].source, EscalationSource::Budget);
        assert_eq!(seen[0].repo, "o/n");
        assert_eq!(seen[0].identity.as_deref(), Some("agent-a"));
        assert_eq!(
            seen[0].summary,
            "identity `agent-a` exceeded its per-task sandbox count budget"
        );
        assert_eq!(
            seen[0].evidence,
            vec!["identity `agent-a` exceeded its per-task sandbox count budget: 0 > 0"]
        );
    }

    #[tokio::test]
    async fn the_per_day_ceiling_blocks_an_identity_across_repositories() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_day: Some(2),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .charge_count("id", "t1", "o/a", BudgetClass::Ci, now())
            .await
            .unwrap();
        accountant
            .charge_count("id", "t2", "o/b", BudgetClass::Ci, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t3", "o/c", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.scope, BudgetScope::Day);
        assert_eq!(err.dimension, BudgetDimension::Count);
    }

    #[tokio::test]
    async fn minutes_exactly_at_a_ceiling_block_the_next_charge_but_do_not_escalate() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(10.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let (accountant, sink) = capturing_accountant(config);
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 10.0, now())
            .await
            .unwrap();
        assert!(sink.lock().unwrap().is_empty());
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err()
            .exceeded()
            .unwrap()
            .clone();
        assert_eq!(err.dimension, BudgetDimension::Minutes);
        assert_eq!(err.scope, BudgetScope::Task);
    }

    #[tokio::test]
    async fn minutes_past_a_ceiling_escalate_once_per_breached_scope() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(10.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let (accountant, sink) = capturing_accountant(config);
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 10.5, now())
            .await
            .unwrap();
        assert_eq!(sink.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_default_limits_are_pinned() {
        assert_eq!(
            BudgetConfig::default(),
            BudgetConfig {
                ci: BudgetLimits {
                    max_count_per_task: Some(20),
                    max_count_per_day: Some(100),
                    max_minutes_per_task: Some(300.0),
                    max_minutes_per_day: Some(600.0),
                },
                sandbox: BudgetLimits {
                    max_count_per_task: Some(3),
                    max_count_per_day: Some(10),
                    max_minutes_per_task: Some(120.0),
                    max_minutes_per_day: Some(240.0),
                },
            }
        );
    }

    #[test]
    fn a_successful_charge_is_persisted_and_a_refused_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let key = ChargeKey {
            identity: "id",
            task_id: "t1",
            repo: "o/n",
            class: BudgetClass::Ci,
            day: now().date_naive(),
        };
        let limits = BudgetLimits {
            max_count_per_task: Some(1),
            ..BudgetLimits::UNLIMITED
        };
        {
            let store = FileBudgetStore::open(&path).unwrap();
            store.try_charge(&key, &limits).unwrap();
            store.try_charge(&key, &limits).unwrap_err();
        }
        let reopened = reopen(&path);
        assert_eq!(reopened.task_usage("id", "t1", BudgetClass::Ci).count, 1);
        assert_eq!(
            reopened.task_usage("id", "t1", BudgetClass::Sandbox).count,
            0
        );
    }

    #[test]
    fn opening_a_budget_path_that_cannot_be_read_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(
            FileBudgetStore::open(&path),
            Err(BudgetStoreError::Io { .. })
        ));
    }

    #[test]
    fn a_lock_attempt_maps_to_ok_locked_or_io() {
        let path = std::path::Path::new("b.json");
        let lock = std::path::Path::new("b.json.lock");
        assert!(lock_outcome(path, lock, Ok(())).is_ok());
        assert!(matches!(
            lock_outcome(path, lock, Err(fs4::TryLockError::WouldBlock)),
            Err(BudgetStoreError::Locked { path: p }) if p == path
        ));
        let failure = std::io::Error::other("flock failed");
        assert!(matches!(
            lock_outcome(path, lock, Err(fs4::TryLockError::Error(failure))),
            Err(BudgetStoreError::Io { path: p, .. }) if p == lock
        ));
    }

    #[test]
    fn opening_when_the_lock_file_cannot_be_created_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        std::fs::create_dir(dir.path().join("budget.json.lock")).unwrap();
        assert!(matches!(
            FileBudgetStore::open(&path),
            Err(BudgetStoreError::Io { path: p, .. }) if p.ends_with("budget.json.lock")
        ));
    }

    #[tokio::test]
    async fn the_file_store_records_and_reloads_minutes_per_task_day_and_repo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        {
            let accountant = CostAccountant::new(
                Arc::new(FileBudgetStore::open(&path).unwrap()),
                BudgetConfig::UNLIMITED,
            );
            accountant
                .record_minutes("id", "t1", "o/n", BudgetClass::Sandbox, 7.5, now())
                .await
                .unwrap();
        }
        let reopened = reopen(&path);
        let day = now().date_naive();
        assert_eq!(
            reopened
                .task_usage("id", "t1", BudgetClass::Sandbox)
                .minutes,
            7.5
        );
        assert_eq!(
            reopened.day_usage("id", BudgetClass::Sandbox, day).minutes,
            7.5
        );
        assert_eq!(
            reopened
                .repo_day_usage("o/n", BudgetClass::Sandbox, day)
                .minutes,
            7.5
        );
        assert_eq!(reopened.day_usage("id", BudgetClass::Ci, day).minutes, 0.0);
    }

    fn key_for(identity: &'static str, task_id: &'static str) -> ChargeKey<'static> {
        ChargeKey {
            identity,
            task_id,
            repo: "o/n",
            class: BudgetClass::Ci,
            day: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        }
    }

    fn minutes(m: f64) -> Usage {
        Usage {
            count: 0,
            minutes: m,
        }
    }

    #[test]
    fn invalid_minutes_are_rejected_without_a_charge_and_leave_the_file_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        {
            let store = FileBudgetStore::open(&path).unwrap();
            store.record(&key_for("id", "t1"), minutes(299.0)).unwrap();
            for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1000.0, -0.5] {
                let err = store
                    .record(&key_for("id", "t1"), minutes(bad))
                    .unwrap_err();
                assert!(
                    matches!(err, BudgetError::InvalidMinutes(m) if m.to_bits() == bad.to_bits())
                );
                assert_eq!(store.task_usage("id", "t1", BudgetClass::Ci).minutes, 299.0);
            }
        }
        let reopened = reopen(&path);
        assert_eq!(
            reopened.task_usage("id", "t1", BudgetClass::Ci).minutes,
            299.0
        );
        assert_eq!(
            reopened
                .day_usage("id", BudgetClass::Ci, key_for("id", "t1").day)
                .minutes,
            299.0
        );
    }

    #[tokio::test]
    async fn the_accountant_refuses_invalid_minutes_and_a_ceiling_still_holds() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_minutes_per_task: Some(10.0),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let (accountant, sink) = capturing_accountant(config);
        for bad in [f64::NAN, f64::INFINITY, -5.0] {
            let err = accountant
                .record_minutes("id", "t1", "o/n", BudgetClass::Ci, bad, now())
                .await
                .unwrap_err();
            assert!(matches!(err, BudgetError::InvalidMinutes(_)));
            let err = accountant
                .record_minutes_sync("id", "t1", "o/n", BudgetClass::Ci, bad, now())
                .unwrap_err();
            assert!(matches!(err, BudgetError::InvalidMinutes(_)));
        }
        assert_eq!(accountant.task_summary("id", "t1"), BudgetReport::default());
        assert!(sink.lock().unwrap().is_empty());
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 50.0, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err();
        assert_eq!(err.exceeded().unwrap().dimension, BudgetDimension::Minutes);
    }

    #[test]
    fn counts_saturate_instead_of_overflowing() {
        let store = InMemoryBudgetStore::new();
        let big = Usage {
            count: u32::MAX,
            minutes: 0.0,
        };
        store.record(&key_for("id", "t1"), big).unwrap();
        store
            .record(
                &key_for("id", "t1"),
                Usage {
                    count: 5,
                    minutes: 0.0,
                },
            )
            .unwrap();
        assert_eq!(
            store.task_usage("id", "t1", BudgetClass::Ci).count,
            u32::MAX
        );
    }

    #[test]
    fn minutes_saturate_at_the_largest_finite_value() {
        let store = InMemoryBudgetStore::new();
        store
            .record(&key_for("id", "t1"), minutes(f64::MAX))
            .unwrap();
        store
            .record(&key_for("id", "t1"), minutes(f64::MAX))
            .unwrap();
        assert_eq!(
            store.task_usage("id", "t1", BudgetClass::Ci).minutes,
            f64::MAX
        );
    }

    #[test]
    fn a_failed_write_refuses_the_charge_and_rolls_the_ledger_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let store = FileBudgetStore::open(&path).unwrap();
        let blocker = dir.path().join("budget.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        let limits = BudgetLimits {
            max_count_per_task: Some(1),
            ..BudgetLimits::UNLIMITED
        };
        let err = store.try_charge(&key_for("id", "t1"), &limits).unwrap_err();
        assert!(matches!(&err, BudgetError::Persist { path: p, .. } if *p == path));
        assert_eq!(store.task_usage("id", "t1", BudgetClass::Ci).count, 0);
        assert_eq!(
            store
                .day_usage("id", BudgetClass::Ci, key_for("id", "t1").day)
                .count,
            0
        );
        assert!(store.record(&key_for("id", "t1"), minutes(3.0)).is_err());
        assert!(store.note_run("o/n", 1).is_err());
        assert!(store.pending_runs("o/n").is_empty());
        assert!(store.claim_run("o/n", 1).is_err());
        std::fs::remove_dir(&blocker).unwrap();
        store.try_charge(&key_for("id", "t1"), &limits).unwrap();
        assert!(store.try_charge(&key_for("id", "t1"), &limits).is_err());
        drop(store);
        let reopened = reopen(&path);
        assert_eq!(reopened.task_usage("id", "t1", BudgetClass::Ci).count, 1);
    }

    #[tokio::test]
    async fn a_persist_failure_denies_the_charge_without_an_exhaustion_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let sink = Arc::new(Mutex::new(Vec::new()));
        let escalator = Arc::new(Escalator::new(
            Arc::new(EscalationLog::in_memory()),
            Arc::new(FanoutSink(vec![Box::new(CapturingSink(Arc::clone(&sink)))])),
            Arc::new(SystemClock),
            chrono::Duration::hours(1),
        ));
        let accountant = CostAccountant::new(
            Arc::new(FileBudgetStore::open(&path).unwrap()),
            BudgetConfig::UNLIMITED,
        )
        .with_escalator(escalator);
        std::fs::create_dir(dir.path().join("budget.json.tmp")).unwrap();
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err();
        assert!(matches!(err, BudgetError::Persist { .. }));
        assert!(err.exceeded().is_none());
        assert!(sink.lock().unwrap().is_empty());
        assert_eq!(accountant.task_summary("id", "t1"), BudgetReport::default());
    }

    #[cfg(unix)]
    #[test]
    fn the_ledger_file_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let store = FileBudgetStore::open(&path).unwrap();
        store
            .try_charge(&key_for("id", "t1"), &BudgetLimits::UNLIMITED)
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_run_is_claimed_once_on_the_file_store_too() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileBudgetStore::open(dir.path().join("budget.json")).unwrap();
        store.note_run("o/n", 7).unwrap();
        assert_eq!(store.pending_runs("o/n"), vec![7]);
        assert!(store.claim_run("o/n", 7).unwrap());
        assert!(!store.claim_run("o/n", 7).unwrap());
        store.note_run("o/n", 7).unwrap();
        assert!(store.pending_runs("o/n").is_empty());
    }

    #[test]
    fn concurrent_file_store_charges_cannot_overshoot_a_limit_of_one_and_persist_exactly_one() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("budget.json");
            let store = Arc::new(FileBudgetStore::open(&path).unwrap());
            let limits = BudgetLimits {
                max_count_per_task: Some(1),
                ..BudgetLimits::UNLIMITED
            };
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let store = Arc::clone(&store);
                    std::thread::spawn(move || {
                        store.try_charge(&key_for("id", "t1"), &limits).is_ok()
                    })
                })
                .collect();
            let granted = threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(|ok| *ok)
                .count();
            assert_eq!(granted, 1);
            let mut remaining = store;
            let owned = loop {
                match Arc::try_unwrap(remaining) {
                    Ok(owned) => break owned,
                    Err(still_shared) => {
                        remaining = still_shared;
                        std::thread::yield_now();
                    }
                }
            };
            drop(owned);
            let reopened = reopen(&path);
            assert_eq!(reopened.task_usage("id", "t1", BudgetClass::Ci).count, 1);
        }
    }

    #[test]
    fn a_failed_write_restores_the_previous_usage_and_not_an_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.json");
        let store = FileBudgetStore::open(&path).unwrap();
        store
            .try_charge(&key_for("id", "t1"), &BudgetLimits::UNLIMITED)
            .unwrap();
        store.note_run("o/n", 9).unwrap();
        let blocker = dir.path().join("budget.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        assert!(store
            .try_charge(&key_for("id", "t1"), &BudgetLimits::UNLIMITED)
            .is_err());
        assert_eq!(store.task_usage("id", "t1", BudgetClass::Ci).count, 1);
        assert_eq!(store.pending_runs("o/n"), vec![9]);
    }

    #[test]
    fn every_concurrent_file_store_charge_is_persisted() {
        for _ in 0..10 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("budget.json");
            let store = Arc::new(FileBudgetStore::open(&path).unwrap());
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let store = Arc::clone(&store);
                    std::thread::spawn(move || {
                        store
                            .try_charge(&key_for("id", "t1"), &BudgetLimits::UNLIMITED)
                            .unwrap();
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            let mut remaining = store;
            let owned = loop {
                match Arc::try_unwrap(remaining) {
                    Ok(owned) => break owned,
                    Err(still_shared) => {
                        remaining = still_shared;
                        std::thread::yield_now();
                    }
                }
            };
            drop(owned);
            let reopened = reopen(&path);
            assert_eq!(reopened.task_usage("id", "t1", BudgetClass::Ci).count, 8);
        }
    }

    #[test]
    fn identities_and_task_ids_are_exact_case_sensitive_keys() {
        let store = InMemoryBudgetStore::new();
        let day = key_for("id", "t1").day;
        store.record(&key_for("id", "t1"), minutes(1.0)).unwrap();
        store.record(&key_for("Id", "t1"), minutes(2.0)).unwrap();
        store.record(&key_for("other", "t1"), minutes(4.0)).unwrap();
        assert_eq!(store.task_usage("id", "t1", BudgetClass::Ci).minutes, 1.0);
        assert_eq!(store.task_usage("Id", "t1", BudgetClass::Ci).minutes, 2.0);
        assert_eq!(
            store.task_usage("other", "t1", BudgetClass::Ci).minutes,
            4.0
        );
        assert_eq!(
            store.task_usage("ID", "t1", BudgetClass::Ci),
            Usage::default()
        );
        assert_eq!(store.day_usage("id", BudgetClass::Ci, day).minutes, 1.0);
        assert_eq!(
            store.repo_day_usage("o/n", BudgetClass::Ci, day).minutes,
            7.0
        );
    }

    proptest::proptest! {
        #[test]
        fn add_never_decreases_never_nan_and_saturates(
            a in 0.0f64..=f64::MAX,
            b in 0.0f64..=f64::MAX,
            ca in proptest::num::u32::ANY,
            cb in proptest::num::u32::ANY,
        ) {
            let mut usage = Usage { count: ca, minutes: a };
            add(&mut usage, Usage { count: cb, minutes: b });
            proptest::prop_assert!(usage.minutes.is_finite());
            proptest::prop_assert!(usage.minutes >= a && usage.minutes >= b);
            proptest::prop_assert_eq!(usage.count, ca.saturating_add(cb));
            proptest::prop_assert!(usage.count >= ca && usage.count >= cb);
        }

        #[test]
        fn recorded_minutes_are_monotone_finite_and_ignore_invalid_input(
            deltas in proptest::collection::vec(proptest::num::f64::ANY, 0..24),
        ) {
            let store = InMemoryBudgetStore::new();
            let mut last = 0.0f64;
            for delta in deltas {
                let outcome = store.record(&key_for("id", "t1"), minutes(delta));
                let valid = delta.is_finite() && delta >= 0.0;
                proptest::prop_assert_eq!(outcome.is_ok(), valid);
                let now_used = store.task_usage("id", "t1", BudgetClass::Ci).minutes;
                proptest::prop_assert!(now_used.is_finite());
                proptest::prop_assert!(now_used >= last);
                last = now_used;
            }
        }
    }

    #[test]
    fn narrowing_limits_takes_the_stricter_of_each_dimension_and_never_widens() {
        let host = BudgetLimits {
            max_count_per_task: Some(20),
            max_count_per_day: Some(100),
            max_minutes_per_task: Some(300.0),
            max_minutes_per_day: None,
        };
        let identity = BudgetLimits {
            max_count_per_task: Some(5),
            max_count_per_day: Some(500),
            max_minutes_per_task: None,
            max_minutes_per_day: Some(60.0),
        };
        assert_eq!(
            host.narrowed(&identity),
            BudgetLimits {
                max_count_per_task: Some(5),
                max_count_per_day: Some(100),
                max_minutes_per_task: Some(300.0),
                max_minutes_per_day: Some(60.0),
            }
        );
        assert_eq!(host.narrowed(&BudgetLimits::UNLIMITED), host);
        assert_eq!(BudgetLimits::UNLIMITED.narrowed(&identity), identity);
        assert_eq!(
            host.narrowed(&BudgetLimits::DENY_ALL),
            BudgetLimits::DENY_ALL
        );
    }

    #[tokio::test]
    async fn a_per_identity_ceiling_blocks_earlier_than_the_host_default() {
        let accountant = accountant(BudgetConfig::UNLIMITED);
        let tight = BudgetLimits {
            max_count_per_task: Some(1),
            ..BudgetLimits::UNLIMITED
        };
        accountant
            .charge_count_within(&tight, "id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
        let err = accountant
            .charge_count_within(&tight, "id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err();
        assert_eq!(err.exceeded().unwrap().scope, BudgetScope::Task);
        accountant
            .charge_count("id", "t2", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_per_identity_ceiling_cannot_widen_the_accountants_own_config() {
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(1),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = accountant(config);
        accountant
            .charge_count_within(
                &BudgetLimits::UNLIMITED,
                "id",
                "t1",
                "o/n",
                BudgetClass::Ci,
                now(),
            )
            .await
            .unwrap();
        assert!(accountant
            .charge_count_within(
                &BudgetLimits::UNLIMITED,
                "id",
                "t1",
                "o/n",
                BudgetClass::Ci,
                now()
            )
            .await
            .is_err());
    }

    #[test]
    fn a_ledger_file_with_negative_or_non_finite_minutes_is_refused_on_load() {
        let dir = tempfile::tempdir().unwrap();
        for (index, usage) in [
            r#"{"count":1,"minutes":-5.0}"#,
            r#"{"count":1,"minutes":1e999}"#,
            r#"{"count":1,"minutes":null}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let path = dir.path().join(format!("budget{index}.json"));
            let body = format!(
                r#"{{"tasks":[["id","t1","ci",{usage}]],"days":[],"repos":[],"charged_runs":[],"pending_runs":[]}}"#
            );
            std::fs::write(&path, body).unwrap();
            assert!(
                matches!(
                    FileBudgetStore::open(&path),
                    Err(BudgetStoreError::Invalid { .. } | BudgetStoreError::Corrupt { .. })
                ),
                "{usage}"
            );
        }
    }

    #[test]
    fn negative_minutes_in_any_ledger_table_are_refused_on_load_as_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let negative = r#"{"count":0,"minutes":-0.5}"#;
        let ok = r#"{"count":0,"minutes":0.5}"#;
        for (task, day, repo) in [(negative, ok, ok), (ok, negative, ok), (ok, ok, negative)] {
            let path = dir.path().join("budget.json");
            let body = format!(
                r#"{{"tasks":[["id","t1","ci",{task}]],"days":[["id","ci","2026-09-26",{day}]],"repos":[["o/n","ci","2026-09-26",{repo}]],"charged_runs":[],"pending_runs":[]}}"#
            );
            std::fs::write(&path, body).unwrap();
            assert!(matches!(
                FileBudgetStore::open(&path),
                Err(BudgetStoreError::Invalid { .. })
            ));
        }
        let path = dir.path().join("fine.json");
        let body = format!(
            r#"{{"tasks":[["id","t1","ci",{ok}]],"days":[["id","ci","2026-09-26",{ok}]],"repos":[["o/n","ci","2026-09-26",{ok}]],"charged_runs":[],"pending_runs":[]}}"#
        );
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            FileBudgetStore::open(&path)
                .unwrap()
                .task_usage("id", "t1", BudgetClass::Ci)
                .minutes,
            0.5
        );
    }
}
