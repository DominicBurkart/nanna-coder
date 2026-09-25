use super::adapter::{FakeAdapter, TargetAdapter};
use super::health::{check_health, FakeHealthSource, HealthBreach, HealthSource};
use super::hooks::{AuditHook, EscalationHook, LogEscalation, NoAudit, RolloutEscalation};
use super::log::RolloutLog;
use super::state::{RolloutRecord, RolloutState};
use super::RolloutError;
use crate::deploy::{DeployPlan, DeployStep, OnBreach, Precondition, StepKind};
use crate::leases::{
    acquire_all, Clock, InMemoryLeaseStore, LeaseError, LeaseStore, SimulatedClock,
};
use crate::windows::WindowSet;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;

/// Tunables of the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RolloutConfig {
    /// How often health is sampled while a step bakes.
    pub poll_interval: Duration,
    /// Slack added to a step's hold and bake time when leasing the deploy lock.
    pub lease_grace: Duration,
}

impl Default for RolloutConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::minutes(1),
            lease_grace: Duration::hours(1),
        }
    }
}

/// Drives rollouts recorded in a [`RolloutLog`] through their plans.
///
/// The executor holds no state of its own: every call reloads the record
/// from the log and every transition is appended before the next effect
/// runs, so two executors over one log agree and a restart resumes where
/// the last one stopped.
///
/// ```
/// use harness::deploy::DeployTemplate;
/// use harness::rollout::{fake_executor, RolloutLog, RolloutState};
/// use harness::windows::WindowSet;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
/// let plan = DeployTemplate::parse(
///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [10, 100]\nmin_step_duration = \"1h\"\n",
/// )
/// .unwrap()
/// .plan("sandbox")
/// .unwrap();
/// let (executor, adapter, _health) = fake_executor(log, WindowSet::default(), "registry.example.invalid/ns/app:v1", &[]);
/// let record = executor.start(plan, "registry.example.invalid/ns/app:v2").await.unwrap();
/// let done = executor.run(&record.id).await.unwrap();
/// assert_eq!(done.state, RolloutState::Complete);
/// assert_eq!(done.traffic_percent, 100);
/// assert_eq!(adapter.current(), "registry.example.invalid/ns/app:v2");
/// # });
/// ```
pub struct RolloutExecutor {
    log: RolloutLog,
    leases: Arc<dyn LeaseStore>,
    windows: WindowSet,
    clock: Arc<dyn Clock>,
    adapter: Arc<dyn TargetAdapter>,
    health: Arc<dyn HealthSource>,
    audit: Arc<dyn AuditHook>,
    escalation: Arc<dyn EscalationHook>,
    config: RolloutConfig,
}

/// An executor over in-memory leases, a simulated clock started now, a
/// [`FakeAdapter`] serving `current_image` and a healthy
/// [`FakeHealthSource`] for `endpoints`: the `--fake` dry run.
pub fn fake_executor(
    log: RolloutLog,
    windows: WindowSet,
    current_image: &str,
    endpoints: &[String],
) -> (RolloutExecutor, Arc<FakeAdapter>, Arc<FakeHealthSource>) {
    let adapter = Arc::new(FakeAdapter::new(current_image));
    let health = Arc::new(FakeHealthSource::healthy(endpoints));
    let clock = Arc::new(SimulatedClock::new(Utc::now()));
    let leases = Arc::new(InMemoryLeaseStore::default());
    let executor =
        RolloutExecutor::new(log, leases, windows, clock, adapter.clone(), health.clone());
    (executor, adapter, health)
}

impl RolloutExecutor {
    /// An executor with a no-op audit hook and a logging escalation hook.
    pub fn new(
        log: RolloutLog,
        leases: Arc<dyn LeaseStore>,
        windows: WindowSet,
        clock: Arc<dyn Clock>,
        adapter: Arc<dyn TargetAdapter>,
        health: Arc<dyn HealthSource>,
    ) -> Self {
        Self {
            log,
            leases,
            windows,
            clock,
            adapter,
            health,
            audit: Arc::new(NoAudit),
            escalation: Arc::new(LogEscalation),
            config: RolloutConfig::default(),
        }
    }

    /// Replace the audit hook.
    pub fn with_audit(mut self, audit: Arc<dyn AuditHook>) -> Self {
        self.audit = audit;
        self
    }

    /// Replace the escalation hook.
    pub fn with_escalation(mut self, escalation: Arc<dyn EscalationHook>) -> Self {
        self.escalation = escalation;
        self
    }

    /// Replace the tunables.
    pub fn with_config(mut self, config: RolloutConfig) -> Self {
        self.config = config;
        self
    }

    /// The log rollouts are persisted in.
    pub fn log(&self) -> &RolloutLog {
        &self.log
    }

    /// Record a new `Pending` rollout of `image` under `plan`, noting the
    /// image the target serves now as the rollback target.
    pub async fn start(
        &self,
        plan: DeployPlan,
        image: &str,
    ) -> Result<RolloutRecord, RolloutError> {
        let previous = self.adapter.current_image().await?;
        let id = format!("rollout-{}", uuid::Uuid::new_v4().simple());
        let record = RolloutRecord::new(id, plan, image, &previous, self.clock.now());
        record.lease_name()?;
        self.log.append(None, &record)?;
        Ok(record)
    }

    /// The current record of rollout `id`.
    pub fn status(&self, id: &str) -> Result<RolloutRecord, RolloutError> {
        self.log.load(id)
    }

    /// The current record of every rollout, oldest first.
    pub fn list(&self) -> Result<Vec<RolloutRecord>, RolloutError> {
        let mut records: Vec<_> = self.log.latest()?.into_values().collect();
        records.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(records)
    }

    /// Kill switch: hold rollout `id` where it is; see [`RolloutLog::halt`].
    /// Human-only: no agent tool exposes this.
    pub fn halt(&self, id: &str) -> Result<RolloutRecord, RolloutError> {
        self.log.halt(id, self.clock.now())
    }

    /// Restart rollout `id` from step 0 with `image`, the fix that `pr`
    /// delivered. The slot serving the bad image is drained and retired.
    pub async fn roll_forward(
        &self,
        id: &str,
        image: &str,
        pr: Option<&str>,
    ) -> Result<RolloutRecord, RolloutError> {
        let Some(pr) = pr.map(str::trim).filter(|p| !p.is_empty()) else {
            return Err(RolloutError::PrRequired);
        };
        let record = self.log.load(id)?;
        let mut next = record.clone();
        next.transition(RolloutState::Step(0), self.clock.now())?;
        if let Some(slot) = &record.slot {
            self.adapter.set_traffic(slot, 0).await?;
            self.adapter.retire(slot).await?;
        }
        next.image = image.to_string();
        next.pr = Some(pr.to_string());
        next.slot = None;
        next.traffic_percent = 0;
        next.breach = None;
        self.log.append(Some(&record.state), &next)?;
        Ok(next)
    }

    /// Drive rollout `id` until it is complete, rolled back, halted, or
    /// parked on a precondition that is not yet met.
    ///
    /// A parked rollout returns immediately; call again at or after its
    /// `until` to resume. Every effect is preceded by a persisted
    /// transition, so a crash anywhere resumes at the same step.
    pub async fn run(&self, id: &str) -> Result<RolloutRecord, RolloutError> {
        loop {
            let mut record = self.log.load(id)?;
            match record.state.clone() {
                RolloutState::Pending => self.persist(&mut record, RolloutState::Step(0))?,
                RolloutState::Step(n) => self.step(record, n).await?,
                RolloutState::Baking { step, since } => self.bake(record, step, since).await?,
                RolloutState::RollingBack => self.roll_back(record).await?,
                RolloutState::Parked {
                    until,
                    resume_state,
                } => {
                    if self.clock.now() < until {
                        return Ok(record);
                    }
                    self.persist(&mut record, *resume_state)?;
                }
                RolloutState::Complete | RolloutState::RolledBack | RolloutState::Halted => {
                    return Ok(record)
                }
            }
        }
    }

    fn persist(&self, record: &mut RolloutRecord, next: RolloutState) -> Result<(), RolloutError> {
        let from = record.state.clone();
        record.transition(next, self.clock.now())?;
        tracing::info!(rollout = %record.id, from = %from, to = %record.state, traffic = record.traffic_percent, "Rollout transition");
        self.log.append(Some(&from), record)
    }

    fn step_of(record: &RolloutRecord, n: usize) -> Result<DeployStep, RolloutError> {
        record
            .plan
            .steps
            .get(n)
            .cloned()
            .ok_or_else(|| RolloutError::NoSuchStep {
                id: record.id.clone(),
                step: n,
            })
    }

    fn park(&self, mut record: RolloutRecord, until: DateTime<Utc>) -> Result<(), RolloutError> {
        let resume_state = Box::new(record.state.clone());
        self.persist(
            &mut record,
            RolloutState::Parked {
                until,
                resume_state,
            },
        )
    }

    fn unchanged(&self, record: &RolloutRecord) -> Result<bool, RolloutError> {
        Ok(self.log.load(&record.id)?.state == record.state)
    }

    async fn step(&self, mut record: RolloutRecord, n: usize) -> Result<(), RolloutError> {
        let step = Self::step_of(&record, n)?;
        let now = self.clock.now();
        let lease = record.lease_name()?;
        let ttl = step.min_duration + step.bake_time + self.config.lease_grace;
        match acquire_all(&*self.leases, &[lease], &record.id, ttl, now) {
            Ok(_) => {}
            Err(LeaseError::Held { name, by, until }) => {
                tracing::warn!(rollout = %record.id, lease = %name, held_by = %by, %until, "Deploy lease held; parking");
                return self.park(record, until);
            }
            Err(e) => return Err(e.into()),
        }
        for precondition in &step.preconditions {
            let Precondition::WindowOpen(name) = precondition else {
                continue;
            };
            if self.windows.is_open(name, now)? {
                continue;
            }
            let until = self.windows.next_open(name, now)?;
            tracing::warn!(rollout = %record.id, window = %name, %until, "Window closed; parking");
            self.leases.release_all(&record.id)?;
            return self.park(record, until);
        }
        if let Err(denied) = self.audit.review_step(&record, &step).await {
            let summary = format!("audit denied step {n}: {}", denied.reason);
            return self.halt_and_escalate(record, n, summary, None).await;
        }
        let slot = match record.slot.clone() {
            Some(slot) => slot,
            None if n == 0 => {
                let slot = self.adapter.deploy_inactive(&record.image).await?;
                record.slot = Some(slot.clone());
                self.log.append(Some(&record.state), &record)?;
                slot
            }
            None => return Err(RolloutError::NoSlot(record.id.clone())),
        };
        if step.kind != StepKind::Traffic {
            return Err(RolloutError::UnsupportedStep(step.kind.name()));
        }
        record.set_traffic(step.traffic_percent)?;
        self.adapter
            .set_traffic(&slot, step.traffic_percent)
            .await?;
        let since = self.clock.now();
        self.persist(&mut record, RolloutState::Baking { step: n, since })
    }

    async fn bake(
        &self,
        mut record: RolloutRecord,
        n: usize,
        since: DateTime<Utc>,
    ) -> Result<(), RolloutError> {
        let step = Self::step_of(&record, n)?;
        let slot = record
            .slot
            .clone()
            .ok_or_else(|| RolloutError::NoSlot(record.id.clone()))?;
        let bake_end = since + step.bake_time;
        let hold_end = bake_end + step.min_duration;
        loop {
            let now = self.clock.now();
            if now >= bake_end {
                break;
            }
            if let Some(health) = &record.plan.health {
                let sample = self.health.sample(&slot, self.config.poll_interval).await?;
                if let Some(breach) = check_health(health, &sample, n) {
                    return self.on_breach(record, breach).await;
                }
            }
            self.clock
                .sleep(self.config.poll_interval.min(bake_end - now))
                .await;
            if !self.unchanged(&record)? {
                return Ok(());
            }
        }
        let now = self.clock.now();
        if now < hold_end {
            self.clock.sleep(hold_end - now).await;
            if !self.unchanged(&record)? {
                return Ok(());
            }
        }
        if n + 1 < record.plan.steps.len() {
            return self.persist(&mut record, RolloutState::Step(n + 1));
        }
        self.persist(&mut record, RolloutState::Complete)?;
        self.leases.release_all(&record.id)?;
        Ok(())
    }

    async fn on_breach(
        &self,
        mut record: RolloutRecord,
        breach: HealthBreach,
    ) -> Result<(), RolloutError> {
        tracing::warn!(rollout = %record.id, %breach, "Health breach");
        record.breach = Some(breach.clone());
        let policy = record.plan.rollback.clone();
        let step = breach.step;
        if !policy.automatic {
            let summary = format!("{breach}; rollback.automatic is false, a human decides");
            return self
                .halt_and_escalate(record, step, summary, Some(breach))
                .await;
        }
        match policy.on_breach {
            OnBreach::Rollback => self.persist(&mut record, RolloutState::RollingBack),
            OnBreach::RollForward => {
                let summary = format!("{breach}; roll-forward needs a fix: nanna deploy roll-forward {} --image <ref> --pr <url>", record.id);
                self.halt_and_escalate(record, step, summary, Some(breach))
                    .await
            }
            OnBreach::HaltAndEscalate => {
                let summary = breach.to_string();
                self.halt_and_escalate(record, step, summary, Some(breach))
                    .await
            }
        }
    }

    async fn roll_back(&self, mut record: RolloutRecord) -> Result<(), RolloutError> {
        if let Some(slot) = &record.slot {
            self.adapter.set_traffic(slot, 0).await?;
        }
        self.adapter.rollback_to(&record.previous_image).await?;
        record.set_traffic(0)?;
        self.persist(&mut record, RolloutState::RolledBack)?;
        self.leases.release_all(&record.id)?;
        Ok(())
    }

    async fn halt_and_escalate(
        &self,
        mut record: RolloutRecord,
        step: usize,
        summary: String,
        breach: Option<HealthBreach>,
    ) -> Result<(), RolloutError> {
        self.persist(&mut record, RolloutState::Halted)?;
        let escalation = RolloutEscalation {
            rollout_id: record.id.clone(),
            environment: record.plan.environment.clone(),
            image: record.image.clone(),
            previous_image: record.previous_image.clone(),
            step,
            traffic_percent: record.traffic_percent,
            summary,
            breach,
        };
        self.escalation
            .escalate(&escalation)
            .await
            .map_err(RolloutError::Escalation)
    }
}

impl std::fmt::Debug for RolloutExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RolloutExecutor")
            .field("log", &self.log)
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::DeployTemplate;
    use crate::leases::{LeaseName, SimulatedClock};
    use crate::rollout::adapter::{AdapterCall, AdapterOp, Slot};
    use crate::rollout::health::{HealthError, HealthSample};
    use crate::rollout::hooks::{RecordingAudit, RecordingEscalation};
    use crate::rollout::state::tests::{plan, t0, GRADUAL};
    use async_trait::async_trait;
    use chrono::TimeZone;

    const V1: &str = "registry.example.invalid/ns/app:v1";
    const V2: &str = "registry.example.invalid/ns/app:v2";
    const V3: &str = "registry.example.invalid/ns/app:v3";
    const WINDOWS: &str = "[[window]]\nname = \"business-hours\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\"]\nstart = \"09:30\"\nend = \"17:00\"\napplies_to = [\"production\"]\n";

    struct Rig {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
        clock: Arc<SimulatedClock>,
        leases: Arc<InMemoryLeaseStore>,
        adapter: Arc<FakeAdapter>,
        health: Arc<FakeHealthSource>,
        audit: Arc<RecordingAudit>,
        escalation: Arc<RecordingEscalation>,
        executor: RolloutExecutor,
    }

    fn one_poll_per_step() -> RolloutConfig {
        RolloutConfig {
            poll_interval: Duration::minutes(30),
            lease_grace: Duration::hours(1),
        }
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollouts.jsonl");
        let clock = Arc::new(SimulatedClock::new(t0()));
        let leases = Arc::new(InMemoryLeaseStore::default());
        let adapter = Arc::new(FakeAdapter::new(V1));
        let health = Arc::new(FakeHealthSource::healthy(&["/health/v1".to_string()]));
        let audit = Arc::new(RecordingAudit::default());
        let escalation = Arc::new(RecordingEscalation::default());
        let executor = RolloutExecutor::new(
            RolloutLog::open(&path).unwrap(),
            leases.clone(),
            WindowSet::parse(WINDOWS).unwrap(),
            clock.clone(),
            adapter.clone(),
            health.clone(),
        )
        .with_audit(audit.clone())
        .with_escalation(escalation.clone())
        .with_config(one_poll_per_step());
        Rig {
            _dir: dir,
            path,
            clock,
            leases,
            adapter,
            health,
            audit,
            escalation,
            executor,
        }
    }

    fn breach_sample() -> HealthSample {
        HealthSample {
            error_rate: 0.2,
            ..HealthSample::healthy(&["/health/v1".to_string()])
        }
    }

    fn plan_with(rollback: &str) -> DeployPlan {
        let src = GRADUAL.replace(
            "[rollback]\nautomatic = true\non_breach = \"rollback\"\n",
            rollback,
        );
        DeployTemplate::parse(&src)
            .unwrap()
            .plan("sandbox")
            .unwrap()
    }

    fn states(executor: &RolloutExecutor, id: &str) -> Vec<String> {
        executor
            .log()
            .history(id)
            .unwrap()
            .iter()
            .map(|t| t.record.state.name().to_string())
            .collect()
    }

    #[tokio::test]
    async fn gradual_plan_completes_on_the_fake_adapter() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(record.id.starts_with("rollout-"));
        assert_eq!(record.previous_image, V1);
        assert_eq!(record.state, RolloutState::Pending);
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.traffic_percent, 100);
        let slot = Slot::new("slot-1");
        assert_eq!(done.slot, Some(slot.clone()));
        assert_eq!(rig.adapter.current(), V2);
        assert_eq!(rig.adapter.max_traffic(&slot), Some(100));
        assert_eq!(
            rig.adapter.calls(),
            vec![
                AdapterCall::CurrentImage,
                AdapterCall::DeployInactive(V2.into()),
                AdapterCall::SetTraffic(slot.clone(), 10),
                AdapterCall::SetTraffic(slot.clone(), 50),
                AdapterCall::SetTraffic(slot.clone(), 100),
            ]
        );
        assert_eq!(
            states(&rig.executor, &record.id),
            ["pending", "step", "step", "baking", "step", "baking", "step", "baking", "complete"]
        );
        assert_eq!(rig.health.calls().len(), 3);
        assert_eq!(rig.audit.reviews().len(), 3);
        assert!(rig.leases.snapshot().unwrap().is_empty());
        assert_eq!(
            rig.clock.now(),
            t0() + Duration::hours(25) + Duration::minutes(30)
        );
        assert_eq!(rig.executor.list().unwrap().len(), 1);
        assert_eq!(rig.executor.status(&record.id).unwrap(), done);
        assert!(rig.escalation.escalations().is_empty());
        assert!(format!("{:?}", rig.executor).contains("RolloutExecutor"));
    }

    #[tokio::test]
    async fn breach_at_step_3_rolls_back_to_the_previous_image() {
        let rig = rig();
        rig.health.push_after(2, breach_sample());
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        assert_eq!(done.traffic_percent, 0);
        assert_eq!(done.breach.as_ref().unwrap().step, 2);
        assert_eq!(rig.adapter.current(), V1);
        let slot = Slot::new("slot-1");
        assert_eq!(rig.adapter.traffic(&slot), Some(0));
        assert_eq!(rig.adapter.max_traffic(&slot), Some(100));
        let calls = rig.adapter.calls();
        assert_eq!(calls[calls.len() - 2], AdapterCall::SetTraffic(slot, 0));
        assert_eq!(calls[calls.len() - 1], AdapterCall::RollbackTo(V1.into()));
        assert!(states(&rig.executor, &record.id).ends_with(&[
            "baking".into(),
            "rolling-back".into(),
            "rolled-back".into()
        ]));
        assert!(rig.leases.snapshot().unwrap().is_empty());
        assert!(rig.executor.halt(&record.id).is_err());
    }

    #[tokio::test]
    async fn crash_mid_bake_resumes_at_the_same_step_with_the_same_slot() {
        let rig = rig();
        rig.health
            .push(HealthSample::healthy(&["/health/v1".to_string()]));
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.health.set_failing(true);
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert!(matches!(err, RolloutError::Health(HealthError(_))), "{err}");
        let crashed = rig.executor.log().load(&record.id).unwrap();
        let RolloutState::Baking { step: 0, since } = crashed.state else {
            panic!("{}", crashed.state)
        };
        assert_eq!(crashed.traffic_percent, 10);
        let during = Duration::minutes(7);
        rig.clock.advance(during);
        let resumed = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            Arc::new(FakeHealthSource::healthy(&["/health/v1".to_string()])),
        )
        .with_config(one_poll_per_step());
        let done = resumed.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.slot, crashed.slot);
        assert_eq!(
            rig.adapter
                .calls()
                .iter()
                .filter(|c| matches!(c, AdapterCall::DeployInactive(_)))
                .count(),
            1
        );
        assert_eq!(rig.clock.sleeps()[0], Duration::minutes(30) - during);
        assert_eq!(rig.clock.sleeps()[1], Duration::hours(8));
        assert_eq!(since, t0());
    }

    #[tokio::test]
    async fn window_expiry_parks_and_resumes_in_the_next_window() {
        let rig = rig();
        let record = rig.executor.start(plan("production"), V2).await.unwrap();
        let parked = rig.executor.run(&record.id).await.unwrap();
        let tuesday_0930_paris = Utc.with_ymd_and_hms(2026, 9, 29, 7, 30, 0).unwrap();
        assert_eq!(
            parked.state,
            RolloutState::Parked {
                until: tuesday_0930_paris,
                resume_state: Box::new(RolloutState::Step(1))
            }
        );
        assert_eq!(parked.traffic_percent, 10);
        assert!(
            rig.leases.snapshot().unwrap().is_empty(),
            "lease is released while parked"
        );
        assert_eq!(rig.executor.run(&record.id).await.unwrap(), parked);
        rig.clock.advance(tuesday_0930_paris - rig.clock.now());
        let parked_again = rig.executor.run(&record.id).await.unwrap();
        let wednesday_0930_paris = Utc.with_ymd_and_hms(2026, 9, 30, 7, 30, 0).unwrap();
        assert_eq!(
            parked_again.state,
            RolloutState::Parked {
                until: wednesday_0930_paris,
                resume_state: Box::new(RolloutState::Step(2))
            }
        );
        assert_eq!(parked_again.traffic_percent, 50);
        rig.clock.advance(wednesday_0930_paris - rig.clock.now());
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(
            states(&rig.executor, &record.id),
            [
                "pending", "step", "step", "baking", "step", "parked", "step", "baking", "step",
                "parked", "step", "baking", "complete"
            ]
        );
    }

    #[tokio::test]
    async fn a_second_lease_holder_parks_the_executor_until_the_lease_lapses() {
        let rig = rig();
        let other = rig
            .leases
            .acquire(
                &LeaseName::deploy("app", "staging"),
                "task-other",
                Duration::hours(2),
                t0(),
            )
            .unwrap();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let parked = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(
            parked.state,
            RolloutState::Parked {
                until: other.until,
                resume_state: Box::new(RolloutState::Step(0))
            }
        );
        assert!(rig
            .adapter
            .calls()
            .iter()
            .all(|c| *c == AdapterCall::CurrentImage));
        rig.clock.advance(Duration::hours(2));
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(rig.leases.snapshot().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn halt_and_escalate_holds_the_split_and_calls_the_hook() {
        let rig = rig();
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 50);
        assert_eq!(rig.adapter.traffic(&Slot::new("slot-1")), Some(50));
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::RollbackTo(_))));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert_eq!(escalations[0].rollout_id, record.id);
        assert_eq!(escalations[0].step, 1);
        assert_eq!(escalations[0].traffic_percent, 50);
        assert_eq!(escalations[0].image, V2);
        assert_eq!(escalations[0].previous_image, V1);
        assert_eq!(escalations[0].environment, "sandbox");
        assert_eq!(
            escalations[0].summary,
            "step 1: error_rate_max 0.01 breached by error rate 0.2"
        );
        assert_eq!(escalations[0].breach, halted.breach);
        assert_eq!(
            rig.leases.snapshot().unwrap().len(),
            1,
            "halted rollout keeps the deploy lease"
        );
        assert_eq!(rig.executor.run(&record.id).await.unwrap(), halted);
    }

    #[tokio::test]
    async fn manual_rollback_policy_halts_on_breach() {
        let rig = rig();
        rig.health.push(breach_sample());
        let plan = plan_with("[rollback]\nautomatic = false\non_breach = \"rollback\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert!(rig.escalation.escalations()[0]
            .summary
            .ends_with("rollback.automatic is false, a human decides"));
    }

    #[tokio::test]
    async fn roll_forward_policy_halts_until_a_fix_arrives_and_requires_a_pr() {
        let rig = rig();
        rig.health.push(breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"roll-forward\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert!(rig.escalation.escalations()[0].summary.contains(&format!(
            "nanna deploy roll-forward {} --image <ref> --pr <url>",
            record.id
        )));
        assert!(matches!(
            rig.executor
                .roll_forward(&record.id, V3, None)
                .await
                .unwrap_err(),
            RolloutError::PrRequired
        ));
        assert!(matches!(
            rig.executor
                .roll_forward(&record.id, V3, Some("  "))
                .await
                .unwrap_err(),
            RolloutError::PrRequired
        ));
        assert_eq!(rig.executor.status(&record.id).unwrap(), halted);
        let forwarded = rig
            .executor
            .roll_forward(
                &record.id,
                V3,
                Some("https://github.com/example/repo/pull/7"),
            )
            .await
            .unwrap();
        assert_eq!(forwarded.state, RolloutState::Step(0));
        assert_eq!(forwarded.image, V3);
        assert_eq!(forwarded.previous_image, V1);
        assert_eq!(
            forwarded.pr.as_deref(),
            Some("https://github.com/example/repo/pull/7")
        );
        assert_eq!(forwarded.slot, None);
        assert_eq!(forwarded.traffic_percent, 0);
        assert_eq!(forwarded.breach, None);
        let old = Slot::new("slot-1");
        assert!(rig.adapter.calls().ends_with(&[
            AdapterCall::SetTraffic(old.clone(), 0),
            AdapterCall::Retire(old.clone())
        ]));
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.slot, Some(Slot::new("slot-2")));
        assert_eq!(rig.adapter.current(), V3);
        assert!(matches!(
            rig.executor
                .roll_forward(&record.id, V3, Some("pr"))
                .await
                .unwrap_err(),
            RolloutError::InvalidTransition { .. }
        ));
    }

    #[tokio::test]
    async fn roll_forward_from_a_step_without_a_slot_skips_the_drain() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.halt(&record.id).unwrap();
        let forwarded = rig
            .executor
            .roll_forward(&record.id, V3, Some("pr-1"))
            .await
            .unwrap();
        assert_eq!(forwarded.state, RolloutState::Step(0));
        assert_eq!(rig.adapter.calls(), vec![AdapterCall::CurrentImage]);
    }

    #[tokio::test]
    async fn audit_denial_halts_and_escalates() {
        let rig = rig();
        rig.audit.deny_from(1, "blast radius exceeds the card");
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert_eq!(halted.breach, None);
        assert_eq!(
            rig.escalation.escalations()[0].summary,
            "audit denied step 1: blast radius exceeds the card"
        );
        assert_eq!(rig.escalation.escalations()[0].step, 1);
    }

    #[tokio::test]
    async fn escalation_failure_is_reported_after_the_halt_is_persisted() {
        let rig = rig();
        rig.escalation.set_failing(true);
        rig.audit.deny_from(0, "no");
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "rollout halted but escalation failed: scripted failure"
        );
        assert_eq!(
            rig.executor.status(&record.id).unwrap().state,
            RolloutState::Halted
        );
    }

    struct HaltOnPoll {
        log: RolloutLog,
        after: usize,
        polls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl HealthSource for HaltOnPoll {
        async fn sample(
            &self,
            _slot: &Slot,
            _window: Duration,
        ) -> Result<HealthSample, HealthError> {
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            if *polls == self.after {
                let record = self.log.latest().unwrap().into_values().next().unwrap();
                let mut halted = record.clone();
                halted
                    .transition(RolloutState::Halted, record.updated_at)
                    .unwrap();
                self.log.append(Some(&record.state), &halted).unwrap();
            }
            Ok(HealthSample::healthy(&["/health/v1".to_string()]))
        }
    }

    #[tokio::test]
    async fn operator_halt_mid_bake_is_noticed_at_the_next_poll() {
        let rig = rig();
        let log = RolloutLog::open(&rig.path).unwrap();
        let health = Arc::new(HaltOnPoll {
            log: log.clone(),
            after: 2,
            polls: std::sync::Mutex::new(0),
        });
        let executor = RolloutExecutor::new(
            log,
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            health,
        )
        .with_config(RolloutConfig {
            poll_interval: Duration::minutes(10),
            lease_grace: Duration::hours(1),
        });
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert_eq!(rig.adapter.traffic(&Slot::new("slot-1")), Some(10));
        assert_eq!(
            rig.clock.sleeps(),
            vec![Duration::minutes(10), Duration::minutes(10)]
        );
    }

    struct HaltOnSleep {
        inner: Arc<SimulatedClock>,
        log: RolloutLog,
        on: usize,
        sleeps: std::sync::Mutex<usize>,
    }

    impl Clock for HaltOnSleep {
        fn now(&self) -> DateTime<Utc> {
            self.inner.now()
        }

        fn sleep(&self, duration: Duration) -> crate::leases::SleepFuture<'_> {
            let mut sleeps = self.sleeps.lock().unwrap();
            *sleeps += 1;
            if *sleeps == self.on {
                let record = self.log.latest().unwrap().into_values().next().unwrap();
                let mut halted = record.clone();
                halted
                    .transition(RolloutState::Halted, record.updated_at)
                    .unwrap();
                self.log.append(Some(&record.state), &halted).unwrap();
            }
            self.inner.sleep(duration)
        }
    }

    #[tokio::test]
    async fn operator_halt_during_the_hold_stops_the_advance() {
        let rig = rig();
        let log = RolloutLog::open(&rig.path).unwrap();
        let clock = Arc::new(HaltOnSleep {
            inner: rig.clock.clone(),
            log: log.clone(),
            on: 2,
            sleeps: std::sync::Mutex::new(0),
        });
        let executor = RolloutExecutor::new(
            log,
            rig.leases.clone(),
            WindowSet::default(),
            clock,
            rig.adapter.clone(),
            rig.health.clone(),
        )
        .with_config(one_poll_per_step());
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(
            states(&executor, &record.id),
            ["pending", "step", "step", "baking", "halted"]
        );
    }

    #[tokio::test]
    async fn adapter_failures_surface_and_the_next_run_retries_the_step() {
        let rig = rig();
        rig.adapter.fail(AdapterOp::DeployInactive);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter deploy_inactive failed: scripted failure"
        );
        assert_eq!(
            rig.executor.status(&record.id).unwrap().state,
            RolloutState::Step(0)
        );
        rig.adapter.succeed(AdapterOp::DeployInactive);
        rig.adapter.fail(AdapterOp::SetTraffic);
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert!(matches!(err, RolloutError::Adapter(_)));
        let record = rig.executor.status(&record.id).unwrap();
        assert_eq!(record.state, RolloutState::Step(0));
        assert_eq!(
            record.slot,
            Some(Slot::new("slot-1")),
            "the deployed slot is persisted before traffic is applied"
        );
        rig.adapter.succeed(AdapterOp::SetTraffic);
        assert_eq!(
            rig.executor.run(&record.id).await.unwrap().state,
            RolloutState::Complete
        );
        assert_eq!(
            rig.adapter
                .calls()
                .iter()
                .filter(|c| matches!(c, AdapterCall::DeployInactive(_)))
                .count(),
            2
        );
        rig.adapter.fail(AdapterOp::CurrentImage);
        assert!(matches!(
            rig.executor.start(plan("sandbox"), V2).await.unwrap_err(),
            RolloutError::Adapter(_)
        ));
    }

    #[tokio::test]
    async fn rollback_in_progress_is_resumed() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let slot = rig.adapter.deploy_inactive(V2).await.unwrap();
        let mut crafted = record.clone();
        crafted.state = RolloutState::RollingBack;
        crafted.slot = Some(slot.clone());
        rig.executor
            .log()
            .append(Some(&record.state), &crafted)
            .unwrap();
        rig.adapter.fail(AdapterOp::RollbackTo);
        assert!(rig.executor.run(&record.id).await.is_err());
        assert_eq!(
            rig.executor.status(&record.id).unwrap().state,
            RolloutState::RollingBack
        );
        rig.adapter.succeed(AdapterOp::RollbackTo);
        assert_eq!(
            rig.executor.run(&record.id).await.unwrap().state,
            RolloutState::RolledBack
        );
        assert!(rig.adapter.calls().ends_with(&[
            AdapterCall::SetTraffic(slot, 0),
            AdapterCall::RollbackTo(V1.into())
        ]));
    }

    #[tokio::test]
    async fn corrupt_records_are_refused_not_guessed() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let mut crafted = record.clone();
        crafted.state = RolloutState::Step(1);
        rig.executor.log().append(None, &crafted).unwrap();
        assert!(
            matches!(rig.executor.run(&record.id).await.unwrap_err(), RolloutError::NoSlot(id) if id == record.id)
        );
        crafted.state = RolloutState::Step(7);
        rig.executor.log().append(None, &crafted).unwrap();
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoSuchStep { step: 7, .. }
        ));
        crafted.state = RolloutState::Baking {
            step: 9,
            since: t0(),
        };
        rig.executor.log().append(None, &crafted).unwrap();
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoSuchStep { step: 9, .. }
        ));
        crafted.state = RolloutState::Baking {
            step: 0,
            since: t0(),
        };
        rig.executor.log().append(None, &crafted).unwrap();
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoSlot(_)
        ));
        assert!(matches!(
            rig.executor.run("rollout-nope").await.unwrap_err(),
            RolloutError::UnknownRollout(_)
        ));
        assert!(matches!(
            rig.executor.halt("rollout-nope").unwrap_err(),
            RolloutError::UnknownRollout(_)
        ));
    }

    #[tokio::test]
    async fn start_refuses_a_plan_with_a_malformed_lease() {
        let rig = rig();
        let mut bad = plan("sandbox");
        bad.lease = "nonsense".into();
        assert!(matches!(
            rig.executor.start(bad, V2).await.unwrap_err(),
            RolloutError::BadLeaseName(_)
        ));
        assert!(rig.executor.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn blue_green_steps_are_not_supported_yet() {
        let rig = rig();
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "step kind `swap` is not supported by the rollout executor yet"
        );
    }

    #[tokio::test]
    async fn unknown_window_and_lease_store_errors_surface() {
        let rig = rig();
        let executor = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            rig.health.clone(),
        );
        let record = executor.start(plan("production"), V2).await.unwrap();
        assert!(matches!(
            executor.run(&record.id).await.unwrap_err(),
            RolloutError::Window(_)
        ));
        let zero_ttl = RolloutConfig {
            poll_interval: Duration::minutes(1),
            lease_grace: Duration::hours(-9),
        };
        let executor = executor.with_config(zero_ttl);
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(matches!(
            executor.run(&record.id).await.unwrap_err(),
            RolloutError::Lease(LeaseError::NonPositiveTtl(_))
        ));
    }

    #[tokio::test]
    async fn list_orders_by_creation_and_fake_executor_dry_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let (executor, adapter, health) =
            fake_executor(log, WindowSet::default(), V1, &["/health/v1".to_string()]);
        let first = executor.start(plan("sandbox"), V2).await.unwrap();
        let second = executor.start(plan("sandbox"), V3).await.unwrap();
        let ids: Vec<_> = executor.list().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&first.id) && ids.contains(&second.id));
        assert_eq!(
            executor.run(&first.id).await.unwrap().state,
            RolloutState::Complete
        );
        assert_eq!(adapter.current(), V2);
        assert_eq!(health.calls().len(), 3 * 30);
    }
}
