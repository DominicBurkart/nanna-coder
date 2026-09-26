//! Per-identity cost accounting for `Ci` and `Sandbox` class actions.
//!
//! Triggering CI or deploying a sandbox costs real money and shared
//! capacity, so [`CostAccountant`] enforces a per-task and per-day ceiling
//! on both a raw call count and, once a provider reveals it, minutes spent.
//! Exceeding either blocks the action with a typed [`BudgetExceeded`] reason
//! and, when an [`Escalator`] is attached, hands the identity's exhaustion
//! off to a human through the existing [`crate::escalation`] path rather
//! than inventing a parallel one.
//!
//! Limits are fixed, crate-wide defaults per [`BudgetClass`]
//! ([`BudgetConfig::default`]) rather than configurable per identity through
//! the `AgentIdentity` TOML schema: the accounting itself is already keyed
//! per identity name, but wiring a *limit* into the schema would ripple into
//! `crate::identity`'s narrowing (widening) checks and every identity
//! fixture file, disproportionate to this issue's scope. A caller that needs
//! different limits constructs its own [`BudgetConfig`].

use crate::effects::EffectClass;
use crate::escalation::{Escalation, EscalationSource, Escalator, Severity};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// The effect classes cost accounting applies to. `ci_status`/`ci_logs`
/// (and any future read-only Ci/Sandbox tool) do not charge count against
/// either budget: only the effectful trigger points do
/// ([`crate::ci_tools::CiTriggerTool`], `sandbox_deploy`). Minutes are
/// recorded separately, once known, regardless of which tool observed them.
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

    /// The [`BudgetClass`] an [`EffectClass`] is budgeted under, or `None`
    /// for every class this accounting does not cover.
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

/// Which window a limit or a usage figure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// Scoped to one task's lifetime.
    Task,
    /// Scoped to one identity's calendar day (UTC).
    Day,
}

impl fmt::Display for BudgetScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BudgetScope::Task => "per-task",
            BudgetScope::Day => "per-day",
        })
    }
}

/// Which counter a limit or a usage figure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Number of calls.
    Count,
    /// Minutes of provider-reported run/sandbox time.
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

/// Usage accumulated so far for one (identity or task, class) pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub count: u32,
    pub minutes: f64,
}

/// A `Ci`/`Sandbox` budget, checked before each dimension: `None` means
/// unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub max_count_per_task: Option<u32>,
    pub max_count_per_day: Option<u32>,
    pub max_minutes_per_task: Option<f64>,
    pub max_minutes_per_day: Option<f64>,
}

impl BudgetLimits {
    /// No ceiling on any dimension.
    pub const UNLIMITED: Self = Self {
        max_count_per_task: None,
        max_count_per_day: None,
        max_minutes_per_task: None,
        max_minutes_per_day: None,
    };
}

/// Per-[`BudgetClass`] limits a [`CostAccountant`] enforces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub ci: BudgetLimits,
    pub sandbox: BudgetLimits,
}

impl BudgetConfig {
    /// No ceiling on either class: every [`CostAccountant::charge_count`]
    /// call succeeds. Useful for tests and for callers that want only the
    /// bookkeeping (usage visible in [`CostAccountant::task_summary`]) with
    /// no enforcement.
    pub const UNLIMITED: Self = Self {
        ci: BudgetLimits::UNLIMITED,
        sandbox: BudgetLimits::UNLIMITED,
    };

    /// The limits configured for `class`.
    pub const fn limits(&self, class: BudgetClass) -> BudgetLimits {
        match class {
            BudgetClass::Ci => self.ci,
            BudgetClass::Sandbox => self.sandbox,
        }
    }
}

/// Conservative crate-wide defaults: a handful of CI dispatches and at most
/// one sandbox deploy at a time per task, with day ceilings that let a busy
/// identity work across several tasks without either budget ever being the
/// normal path to a human's inbox.
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

/// An identity exceeded a configured budget. Carries enough to render both
/// a tool-facing error and an [`Escalation`] summary.
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

    /// A summary stable across occurrences of the same breach: no `used`/
    /// `limit` figures, which change on every call and would otherwise give
    /// every occurrence its own [`Escalation::dedupe_key`] (dedupe keys off
    /// `source`, `repo` and the summary), defeating collapsing entirely.
    /// The numbers still reach the escalation as evidence.
    pub fn stable_summary(&self) -> String {
        format!(
            "identity `{}` exceeded its {} {} {} budget",
            self.identity, self.scope, self.class, self.dimension
        )
    }
}

/// Usage for one task's `Ci` and `Sandbox` budgets, the shape
/// [`crate::task::TaskResult`] and telemetry expose.
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

    /// Record every field as a gauge, mirroring
    /// [`crate::leases::LeaseSnapshot::record`]'s pattern: a caller that
    /// wants these in its metrics backend calls this explicitly rather than
    /// every task completion recording unconditionally.
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

/// Where [`CostAccountant`] persists usage. In-memory only: cross-task
/// per-day limits therefore span the tasks one process handles, matching
/// [`crate::leases::InMemoryLeaseStore`] and [`crate::escalation::EscalationLog::in_memory`]'s
/// defaults; a durable store is not required by this issue's acceptance
/// criteria and can implement this same trait later.
pub trait BudgetStore: Send + Sync {
    /// Usage recorded for `task_id`'s `class` budget so far.
    fn task_usage(&self, task_id: &str, class: BudgetClass) -> Usage;
    /// Usage recorded for `identity`'s `class` budget on `day` so far.
    fn day_usage(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage;
    /// Add `delta` to both the task's and the identity's day counters.
    fn record(
        &self,
        identity: &str,
        task_id: &str,
        class: BudgetClass,
        day: NaiveDate,
        delta: Usage,
    );
}

#[derive(Debug, Default)]
pub struct InMemoryBudgetStore {
    by_task: Mutex<HashMap<(String, BudgetClass), Usage>>,
    by_day: Mutex<HashMap<(String, BudgetClass, NaiveDate), Usage>>,
}

impl InMemoryBudgetStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn add(usage: &mut Usage, delta: Usage) {
    usage.count += delta.count;
    usage.minutes += delta.minutes;
}

impl BudgetStore for InMemoryBudgetStore {
    fn task_usage(&self, task_id: &str, class: BudgetClass) -> Usage {
        self.by_task
            .lock()
            .expect("budget task table poisoned")
            .get(&(task_id.to_string(), class))
            .copied()
            .unwrap_or_default()
    }

    fn day_usage(&self, identity: &str, class: BudgetClass, day: NaiveDate) -> Usage {
        self.by_day
            .lock()
            .expect("budget day table poisoned")
            .get(&(identity.to_string(), class, day))
            .copied()
            .unwrap_or_default()
    }

    fn record(
        &self,
        identity: &str,
        task_id: &str,
        class: BudgetClass,
        day: NaiveDate,
        delta: Usage,
    ) {
        add(
            self.by_task
                .lock()
                .expect("budget task table poisoned")
                .entry((task_id.to_string(), class))
                .or_default(),
            delta,
        );
        add(
            self.by_day
                .lock()
                .expect("budget day table poisoned")
                .entry((identity.to_string(), class, day))
                .or_default(),
            delta,
        );
    }
}

/// Checks and records `Ci`/`Sandbox` usage against a [`BudgetConfig`],
/// escalating exhaustion through an optional [`Escalator`] rather than
/// inventing a new hand-off path.
pub struct CostAccountant {
    store: Arc<dyn BudgetStore>,
    config: BudgetConfig,
    escalator: Option<Arc<Escalator>>,
}

impl CostAccountant {
    pub fn new(store: Arc<dyn BudgetStore>, config: BudgetConfig) -> Self {
        Self {
            store,
            config,
            escalator: None,
        }
    }

    /// Hand budget exhaustion to `escalator` (`Severity::Blocked`,
    /// [`EscalationSource::Budget`]) in addition to returning the typed
    /// [`BudgetExceeded`] reason. Without one, the action is still blocked;
    /// only the human hand-off is skipped, matching how
    /// [`crate::task::TaskManager::with_action_gate`] makes its own opt-in
    /// escalation, rather than one, TaskManager's default.
    pub fn with_escalator(mut self, escalator: Arc<Escalator>) -> Self {
        self.escalator = Some(escalator);
        self
    }

    /// Usage recorded so far for `task_id` across both budgets, for
    /// [`crate::task::TaskResult`] and telemetry.
    pub fn task_summary(&self, task_id: &str) -> BudgetReport {
        BudgetReport {
            ci: self.store.task_usage(task_id, BudgetClass::Ci),
            sandbox: self.store.task_usage(task_id, BudgetClass::Sandbox),
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

    /// Check `class`'s count and minutes-so-far ceilings for `identity`/
    /// `task_id` and, if within them, record one more call. Called before
    /// an expensive trigger (`ci_trigger`, `sandbox_deploy`); read-only
    /// polling tools never call this.
    pub async fn charge_count(
        &self,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        now: DateTime<Utc>,
    ) -> Result<BudgetReport, BudgetExceeded> {
        let limits = self.config.limits(class);
        let day = now.date_naive();
        let task = self.store.task_usage(task_id, class);
        if let Some(max) = limits.max_count_per_task {
            if task.count >= max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Task,
                    BudgetDimension::Count,
                    f64::from(task.count),
                    f64::from(max),
                );
                return Err(self.deny(repo, exceeded).await);
            }
        }
        if let Some(max) = limits.max_minutes_per_task {
            if task.minutes >= max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Task,
                    BudgetDimension::Minutes,
                    task.minutes,
                    max,
                );
                return Err(self.deny(repo, exceeded).await);
            }
        }
        let day_usage = self.store.day_usage(identity, class, day);
        if let Some(max) = limits.max_count_per_day {
            if day_usage.count >= max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Day,
                    BudgetDimension::Count,
                    f64::from(day_usage.count),
                    f64::from(max),
                );
                return Err(self.deny(repo, exceeded).await);
            }
        }
        if let Some(max) = limits.max_minutes_per_day {
            if day_usage.minutes >= max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Day,
                    BudgetDimension::Minutes,
                    day_usage.minutes,
                    max,
                );
                return Err(self.deny(repo, exceeded).await);
            }
        }
        self.store.record(
            identity,
            task_id,
            class,
            day,
            Usage {
                count: 1,
                minutes: 0.0,
            },
        );
        Ok(self.task_summary(task_id))
    }

    /// Like [`Self::record_minutes`], but synchronous and without an
    /// exhaustion check or escalation: bookkeeping only, for a caller that
    /// cannot await (`TaskWorkspace::cleanup`'s sandbox-teardown safety net,
    /// which runs on a task whose `Sandbox`-class tools already escalate on
    /// every call under the default rule auditor, so the next
    /// [`Self::charge_count`] blocks and escalates regardless).
    pub fn record_minutes_sync(
        &self,
        identity: &str,
        task_id: &str,
        class: BudgetClass,
        minutes: f64,
        now: DateTime<Utc>,
    ) -> BudgetReport {
        self.store.record(
            identity,
            task_id,
            class,
            now.date_naive(),
            Usage { count: 0, minutes },
        );
        self.task_summary(task_id)
    }

    /// Add `minutes` to `identity`/`task_id`'s `class` usage once a provider
    /// reveals it (a completed CI run, a torn-down sandbox's lifetime).
    /// Never blocks the call that observed the duration -- the time already
    /// elapsed -- but escalates when it pushes either minutes ceiling past
    /// its limit, so the *next* [`Self::charge_count`] call blocks instead
    /// of silently continuing over budget.
    pub async fn record_minutes(
        &self,
        identity: &str,
        task_id: &str,
        repo: &str,
        class: BudgetClass,
        minutes: f64,
        now: DateTime<Utc>,
    ) -> BudgetReport {
        let day = now.date_naive();
        self.store
            .record(identity, task_id, class, day, Usage { count: 0, minutes });
        let limits = self.config.limits(class);
        let task = self.store.task_usage(task_id, class);
        if let Some(max) = limits.max_minutes_per_task {
            if task.minutes > max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Task,
                    BudgetDimension::Minutes,
                    task.minutes,
                    max,
                );
                self.deny(repo, exceeded).await;
            }
        }
        let day_usage = self.store.day_usage(identity, class, day);
        if let Some(max) = limits.max_minutes_per_day {
            if day_usage.minutes > max {
                let exceeded = BudgetExceeded::new(
                    identity,
                    class,
                    BudgetScope::Day,
                    BudgetDimension::Minutes,
                    day_usage.minutes,
                    max,
                );
                self.deny(repo, exceeded).await;
            }
        }
        self.task_summary(task_id)
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
        let summary = accountant.task_summary("t1");
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
            .unwrap_err();
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
    async fn per_day_count_ceiling_spans_tasks_for_the_same_identity_but_not_others() {
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
            .unwrap_err();
        assert_eq!(err.scope, BudgetScope::Day);
        assert_eq!(err.dimension, BudgetDimension::Count);
        accountant
            .charge_count("other", "t3", "o/n", BudgetClass::Ci, now())
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
            .await;
        let err = accountant
            .charge_count("id", "t1", "o/n", BudgetClass::Ci, now())
            .await
            .unwrap_err();
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
            .await;
        let err = accountant
            .charge_count("id", "t2", "o/n", BudgetClass::Sandbox, now())
            .await
            .unwrap_err();
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
            .await;
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
        let summary = accountant.record_minutes_sync("id", "t1", BudgetClass::Sandbox, 50.0, now());
        assert_eq!(summary.sandbox.minutes, 50.0);
        assert_eq!(accountant.task_summary("t1").sandbox.minutes, 50.0);
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
        let summary = accountant.task_summary("t1");
        assert_eq!(summary.ci.count, 1);
        assert_eq!(summary.sandbox.count, 1);
        let empty = accountant.task_summary("unknown");
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
            .unwrap_err();
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
            .await;
        accountant
            .record_minutes("id", "t1", "o/n", BudgetClass::Ci, 5.0, now())
            .await;
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
            .await;
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
            .unwrap_err();
        assert_eq!(err.dimension, BudgetDimension::Count);
    }

    #[test]
    fn budget_report_json_and_telemetry_recording() {
        let report = BudgetReport {
            ci: Usage {
                count: 3,
                minutes: 12.5,
            },
            sandbox: Usage {
                count: 1,
                minutes: 4.0,
            },
        };
        let json = report.to_json();
        assert_eq!(json["ci"]["count"], 3);
        assert_eq!(json["sandbox"]["minutes"], 4.0);
        let telemetry = crate::telemetry::TelemetrySystem::new();
        report.record(&telemetry);
        assert!(telemetry.get_buffered_metrics_count() > 0);
    }

    #[test]
    fn in_memory_store_isolates_tasks_days_and_identities() {
        let store = InMemoryBudgetStore::new();
        let day = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .unwrap()
            .date_naive();
        store.record(
            "id",
            "t1",
            BudgetClass::Ci,
            day,
            Usage {
                count: 1,
                minutes: 2.0,
            },
        );
        assert_eq!(
            store.task_usage("t1", BudgetClass::Ci),
            Usage {
                count: 1,
                minutes: 2.0
            }
        );
        assert_eq!(
            store.task_usage("t1", BudgetClass::Sandbox),
            Usage::default()
        );
        assert_eq!(store.task_usage("other", BudgetClass::Ci), Usage::default());
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
}
