use super::health::{check_health, FakeHealthSource, HealthBreach, HealthError, HealthSource};
use super::hooks::{AuditHook, EscalationHook, LogEscalation, NoAudit, RolloutEscalation};
use super::incident::{Incident, IncidentResponder, ProposedAction};
use super::log::RolloutLog;
use super::shadow::{FakeShadowSource, NoShadowSource, ShadowComparator, ShadowSource};
use super::state::{RolloutRecord, RolloutState};
use super::RolloutError;
use crate::deploy::{
    DeployPlan, DeployStep, Enforcement, OnBreach, Precondition, PreconditionKind, StepKind,
};
use crate::escalation::{EscalationLog, ResolveGrant};
use crate::leases::{
    acquire_all, Clock, InMemoryLeaseStore, LeaseError, LeaseStore, SimulatedClock,
};
use crate::rollout::adapter::{FakeAdapter, FallbackPolicy, Slot, TargetAdapter};
use crate::windows::WindowSet;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;

const LIVE_SPLIT_LEASE_TTL: Duration = Duration::days(36_500);

enum Verdict {
    Healthy,
    Breach(HealthBreach),
    Unavailable(HealthError),
}

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
/// let (executor, adapter, _health, _shadow, _clock) = fake_executor(log, WindowSet::default(), "registry.example.invalid/ns/app:v1", &[]);
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
    shadow: Arc<dyn ShadowSource>,
    audit: Arc<dyn AuditHook>,
    escalation: Arc<dyn EscalationHook>,
    incident_responder: Option<Arc<IncidentResponder>>,
    production_hold: Option<(Arc<EscalationLog>, Option<String>)>,
    config: RolloutConfig,
}

/// An executor over in-memory leases, a simulated clock started now, a
/// [`FakeAdapter`] serving `current_image`, a healthy [`FakeHealthSource`]
/// for `endpoints` and a [`FakeShadowSource`] whose mirrored pairs always
/// agree: the `--fake` dry run. The returned clock drives [`run_simulated`].
pub fn fake_executor(
    log: RolloutLog,
    windows: WindowSet,
    current_image: &str,
    endpoints: &[String],
) -> (
    RolloutExecutor,
    Arc<FakeAdapter>,
    Arc<FakeHealthSource>,
    Arc<FakeShadowSource>,
    Arc<SimulatedClock>,
) {
    let adapter = Arc::new(FakeAdapter::new(current_image));
    let health = Arc::new(FakeHealthSource::healthy(endpoints));
    let shadow = Arc::new(FakeShadowSource::agreeing(1));
    let clock = Arc::new(SimulatedClock::new(Utc::now()));
    let leases = Arc::new(InMemoryLeaseStore::default());
    let executor = RolloutExecutor::new(
        log,
        leases,
        windows,
        clock.clone(),
        adapter.clone(),
        health.clone(),
    )
    .with_shadow_source(shadow.clone());
    (executor, adapter, health, shadow, clock)
}

/// Drive `id` to a non-parked state on `clock`, advancing it past every
/// `Parked { until, .. }` in between and collecting each intermediate
/// record along the way.
///
/// A rollout parks only on a precondition that is not yet met (a window,
/// a contended lease, or a `Retire` step's `rollback.retain_for`); on a
/// [`SimulatedClock`] there is nothing else that would make it become met
/// on its own, so this always terminates: [`RolloutExecutor::run`] only
/// returns a `Parked` record when `now < until`, and this advances the
/// clock to exactly `until` before running again.
///
/// ```
/// use harness::deploy::DeployTemplate;
/// use harness::rollout::{fake_executor, run_simulated, RolloutLog, RolloutState};
/// use harness::windows::WindowSet;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
/// let plan = DeployTemplate::parse(
///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n",
/// )
/// .unwrap()
/// .plan("sandbox")
/// .unwrap();
/// let (executor, _adapter, _health, _shadow, clock) =
///     fake_executor(log, WindowSet::default(), "registry.example.invalid/ns/app:v1", &[]);
/// let record = executor.start(plan, "registry.example.invalid/ns/app:v2").await.unwrap();
/// let steps = run_simulated(&executor, &clock, &record.id).await.unwrap();
/// assert!(matches!(steps[0].state, RolloutState::Parked { .. }));
/// assert_eq!(steps.last().unwrap().state, RolloutState::Complete);
/// # });
/// ```
pub async fn run_simulated(
    executor: &RolloutExecutor,
    clock: &SimulatedClock,
    id: &str,
) -> Result<Vec<RolloutRecord>, RolloutError> {
    let mut records = Vec::new();
    let mut record = executor.run(id).await?;
    while let RolloutState::Parked { until, .. } = record.state {
        records.push(record);
        clock.advance(until - clock.now());
        record = executor.run(id).await?;
    }
    records.push(record);
    Ok(records)
}

/// Human-only: release the deploy lease a halted rollout keeps while its
/// traffic split is live. The [`ResolveGrant`] is the same proof
/// `nanna escalation resolve` demands, so no agent can free a held split.
///
/// ```
/// use harness::escalation::ResolveGrant;
///
/// assert!(ResolveGrant::check(true, Some("task-1")).is_err());
/// assert!(ResolveGrant::check(true, None).is_ok());
/// ```
pub fn release_halted_lease(
    _grant: &ResolveGrant,
    log: &RolloutLog,
    leases: &dyn LeaseStore,
    id: &str,
) -> Result<RolloutRecord, RolloutError> {
    let record = log.load(id)?;
    if record.state != RolloutState::Halted {
        return Err(RolloutError::NotHalted(id.to_string()));
    }
    leases.release_all(id)?;
    Ok(record)
}

/// [`release_halted_lease`] for `nanna deploy release`: the grant outcome
/// and lease log location are passed in so the command is testable, and the
/// result is the line the command prints.
pub fn release_halted_lease_cli(
    log: &RolloutLog,
    grant: Result<ResolveGrant, crate::escalation::EscalationError>,
    lease_path: Option<std::path::PathBuf>,
    id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let grant = grant?;
    let path = lease_path.ok_or("no lease log location: set NANNA_LEASE_PATH or HOME")?;
    let leases = crate::leases::JsonlLeaseStore::open(&path)?;
    let record = release_halted_lease(&grant, log, &leases, id)?;
    Ok(format!("released {}", record.summary()))
}

/// The precondition kinds [`RolloutExecutor`] evaluates before every step:
/// what plan output may claim is enforced on the run path.
///
/// ```
/// assert!(harness::rollout::enforcement().is_complete());
/// ```
pub fn enforcement() -> Enforcement {
    Enforcement::of(RolloutExecutor::evaluates)
}

impl RolloutExecutor {
    /// Whether `step` evaluates preconditions of `kind`. Exhaustive, so a new
    /// [`PreconditionKind`] forces a decision here and, through
    /// [`enforcement`], in the plan's advisory label.
    pub const fn evaluates(kind: PreconditionKind) -> bool {
        match kind {
            PreconditionKind::Window => true,
            PreconditionKind::Health => true,
            PreconditionKind::Lease => true,
        }
    }

    /// An executor with a no-op audit hook, a logging escalation hook and
    /// no shadow source (a `Shadow` step fails fast until
    /// [`with_shadow_source`](Self::with_shadow_source) is called).
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
            shadow: Arc::new(NoShadowSource),
            audit: Arc::new(NoAudit),
            escalation: Arc::new(LogEscalation),
            incident_responder: None,
            production_hold: None,
            config: RolloutConfig::default(),
        }
    }

    /// Replace the shadow source a `Shadow` step compares against.
    pub fn with_shadow_source(mut self, shadow: Arc<dyn ShadowSource>) -> Self {
        self.shadow = shadow;
        self
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

    /// Give a `halt-and-escalate` breach a chance at automatic remediation
    /// before it reaches a human. With none configured (the default),
    /// every `halt-and-escalate` breach halts and escalates directly,
    /// unchanged from before this existed.
    pub fn with_incident_responder(mut self, responder: Arc<IncidentResponder>) -> Self {
        self.incident_responder = Some(responder);
        self
    }

    /// Wire every stop of this executor to `escalator` and park production
    /// rollouts while the escalator's log holds an incident for `repo`
    /// (`owner/name`, the repository the sinks file in).
    pub fn with_escalator(self, escalator: Arc<crate::escalation::Escalator>, repo: &str) -> Self {
        let log = Arc::clone(escalator.log());
        self.with_escalation(Arc::new(super::EscalatorHook::new(escalator, repo)))
            .with_production_hold(log, Some(repo.to_string()))
    }

    /// Park production rollouts while `log` holds an incident for `repo`
    /// (any repository when `None`).
    pub fn with_production_hold(mut self, log: Arc<EscalationLog>, repo: Option<String>) -> Self {
        self.production_hold = Some((log, repo));
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
        let mut record = RolloutRecord::new(id, plan, image, &previous, self.clock.now());
        record.lease_name()?;
        self.log.append(None, &mut record)?;
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
        let record = self.log.halt(id, self.clock.now())?;
        self.settle_lease(&record)?;
        Ok(record)
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
        next.image = image.to_string();
        next.pr = Some(pr.to_string());
        next.slot = None;
        next.retained_slot = None;
        next.retained_since = None;
        next.fallback = None;
        next.traffic_percent = 0;
        next.breach = None;
        self.log.append(Some(&record.state), &mut next)?;
        if let Some(retained) = &record.retained_slot {
            self.adapter.set_traffic(retained, 100).await?;
        }
        if let Some(slot) = &record.slot {
            self.adapter.set_traffic(slot, 0).await?;
            self.adapter.mirror(slot, 0).await?;
            self.adapter.clear_fallback(slot).await?;
            self.adapter.retire(slot).await?;
        }
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
            let outcome = match record.state.clone() {
                RolloutState::Pending => self.persist(&mut record, RolloutState::Step(0)),
                RolloutState::Step(n) => self.step(record, n).await,
                RolloutState::Baking { step, since } => self.bake(record, step, since).await,
                RolloutState::RollingBack => {
                    let stalled = record.clone();
                    match self.roll_back(record).await {
                        Err(error) if !matches!(error, RolloutError::Conflict { .. }) => {
                            let step = stalled.breach.as_ref().map_or(0, |b| b.step);
                            let summary =
                                format!("rollback failed ({error}): a human must finish it");
                            return Err(self.stop_with(&stalled, step, summary, error).await);
                        }
                        other => other,
                    }
                }
                RolloutState::Parked {
                    until,
                    resume_state,
                } => {
                    if self.clock.now() < until {
                        return Ok(record);
                    }
                    self.persist(&mut record, *resume_state)
                }
                RolloutState::Complete | RolloutState::RolledBack | RolloutState::Halted => {
                    self.settle_lease(&record)?;
                    return Ok(record);
                }
            };
            match outcome {
                Err(RolloutError::Conflict {
                    id,
                    expected,
                    actual,
                }) => {
                    tracing::warn!(rollout = %id, %expected, %actual, "Rollout changed concurrently; re-reading");
                }
                other => other?,
            }
        }
    }

    fn settle_lease(&self, record: &RolloutRecord) -> Result<(), RolloutError> {
        if record.state == RolloutState::Halted && record.traffic_percent > 0 {
            let now = self.clock.now();
            let outcome = acquire_all(
                &*self.leases,
                &[record.lease_name()?],
                &record.id,
                LIVE_SPLIT_LEASE_TTL,
                now,
            );
            if let Err(LeaseError::Held { name, by, .. }) = &outcome {
                tracing::error!(rollout = %record.id, lease = %name, held_by = %by, traffic = record.traffic_percent, "Halted rollout has a live split but another holder owns its deploy lease");
                return Ok(());
            }
            return outcome.map(|_| ()).map_err(RolloutError::from);
        }
        self.leases.release_all(&record.id)?;
        Ok(())
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
        Ok(self.log.load(&record.id)?.revision == record.revision)
    }

    fn guard(&self, record: &RolloutRecord) -> Result<(), RolloutError> {
        let current = self.log.load(&record.id)?;
        if current.revision == record.revision {
            return Ok(());
        }
        Err(RolloutError::Conflict {
            id: record.id.clone(),
            expected: format!("{} at revision {}", record.state, record.revision),
            actual: format!("{} at revision {}", current.state, current.revision),
        })
    }

    async fn step(&self, mut record: RolloutRecord, n: usize) -> Result<(), RolloutError> {
        let step = Self::step_of(&record, n)?;
        let now = self.clock.now();
        if let Some(hold) = self.production_hold_for(&record) {
            tracing::warn!(rollout = %record.id, incident = %hold.escalation_id, "Incident hold active; parking production rollout");
            return self.park(record, now + self.config.poll_interval);
        }
        let lease = record.lease_name()?;
        let ttl = step.min_duration + step.bake_time + self.config.lease_grace;
        let held = match acquire_all(&*self.leases, &[lease], &record.id, ttl, now) {
            Ok(leases) => leases
                .into_iter()
                .map(|l| l.name.to_string())
                .collect::<Vec<_>>(),
            Err(LeaseError::Held { name, by, until }) => {
                tracing::warn!(rollout = %record.id, lease = %name, held_by = %by, %until, "Deploy lease held; parking");
                return self.park(record, until);
            }
            Err(e) => {
                let summary = format!("step {n}: lease store failed ({e})");
                return Err(self.stop_with(&record, n, summary, e.into()).await);
            }
        };
        for precondition in &step.preconditions {
            match precondition {
                Precondition::HealthOk if record.plan.health.is_none() => {
                    let summary = format!(
                        "step {n} requires {precondition} but the plan has no [health] thresholds to evaluate it against"
                    );
                    return self.halt_and_escalate(record, n, summary, None).await;
                }
                Precondition::HealthOk => {}
                Precondition::LeaseHeld(name) => {
                    if !held.contains(name) {
                        let summary = format!(
                            "step {n} requires {precondition} but the executor holds {}",
                            held.join(", ")
                        );
                        return self.halt_and_escalate(record, n, summary, None).await;
                    }
                }
                Precondition::WindowOpen(name) => {
                    let window = self
                        .windows
                        .is_open(name, now)
                        .and_then(|open| Ok((open, self.windows.next_open(name, now)?)));
                    let until = match window {
                        Ok((true, _)) => continue,
                        Ok((false, until)) => until,
                        Err(e) => {
                            let summary = format!("step {n}: window `{name}` unusable ({e})");
                            return Err(self.stop_with(&record, n, summary, e.into()).await);
                        }
                    };
                    tracing::warn!(rollout = %record.id, window = %name, %until, "Window closed; parking");
                    self.leases.release_all(&record.id)?;
                    return self.park(record, until);
                }
            }
        }
        if let Err(denied) = self.audit.review_step(&record, &step).await {
            let summary = format!("audit denied step {n}: {}", denied.reason);
            return self.halt_and_escalate(record, n, summary, None).await;
        }
        let freshly_deployed = record.slot.is_none();
        let slot = match record.slot.clone() {
            Some(slot) => slot,
            None if n == 0 => {
                self.guard(&record)?;
                let slot = self.adapter.deploy_inactive(&record.image).await?;
                record.slot = Some(slot.clone());
                slot
            }
            None => return Err(RolloutError::NoSlot(record.id.clone())),
        };
        if n == 0 && record.fallback.is_none() {
            self.guard(&record)?;
            match self
                .adapter
                .set_fallback(&slot, &FallbackPolicy::default())
                .await
            {
                Ok(support) => record.fallback = Some(support),
                Err(refused) => {
                    if freshly_deployed {
                        let from = record.state.clone();
                        self.log.append(Some(&from), &mut record)?;
                    }
                    return Err(refused.into());
                }
            }
            let from = record.state.clone();
            self.log.append(Some(&from), &mut record)?;
        }
        let retained = if matches!(step.kind, StepKind::Retire) {
            let (retained, retained_since) =
                match (record.retained_slot.clone(), record.retained_since) {
                    (Some(retained), Some(retained_since)) => (retained, retained_since),
                    _ => return Err(RolloutError::NoRetainedSlot(record.id.clone())),
                };
            let ready_at = retained_since + step.min_duration;
            if now < ready_at {
                self.leases.release_all(&record.id)?;
                return self.park(record, ready_at);
            }
            Some(retained)
        } else {
            None
        };
        let sampled = match step.kind {
            StepKind::Traffic | StepKind::Swap => true,
            StepKind::Shadow { .. } | StepKind::Retire => {
                step.preconditions.contains(&Precondition::HealthOk)
            }
        };
        if sampled {
            match self.check(&record, &slot, n).await {
                Verdict::Healthy => {}
                Verdict::Breach(breach) => return self.on_breach(record, breach).await,
                Verdict::Unavailable(error) => {
                    return self.health_unavailable(record, n, error).await
                }
            }
        }
        match step.kind {
            StepKind::Traffic => {
                record.set_traffic(step.traffic_percent)?;
                self.guard(&record)?;
                self.adapter
                    .set_traffic(&slot, step.traffic_percent)
                    .await?;
            }
            StepKind::Shadow { mirror_percent } => {
                record.set_traffic(step.traffic_percent)?;
                self.guard(&record)?;
                self.adapter.mirror(&slot, mirror_percent).await?;
            }
            StepKind::Swap => {
                record.set_traffic(step.traffic_percent)?;
                self.guard(&record)?;
                let swapped = self.adapter.swap().await?;
                record.slot = Some(swapped.active);
                record.retained_slot = Some(swapped.retired_candidate);
                record.retained_since = Some(self.clock.now());
            }
            StepKind::Retire => {
                let retained =
                    retained.ok_or_else(|| RolloutError::NoRetainedSlot(record.id.clone()))?;
                self.guard(&record)?;
                self.adapter.clear_fallback(&retained).await?;
                self.guard(&record)?;
                self.adapter.retire(&retained).await?;
                record.retained_slot = None;
                record.retained_since = None;
                record.set_traffic(step.traffic_percent)?;
                return self.advance_or_complete(record, n).await;
            }
        }
        let since = self.clock.now();
        self.persist(&mut record, RolloutState::Baking { step: n, since })
    }

    async fn advance_or_complete(
        &self,
        mut record: RolloutRecord,
        n: usize,
    ) -> Result<(), RolloutError> {
        if n + 1 < record.plan.steps.len() {
            return self.persist(&mut record, RolloutState::Step(n + 1));
        }
        if let Some(slot) = record.slot.clone() {
            self.guard(&record)?;
            self.adapter.clear_fallback(&slot).await?;
        }
        self.persist(&mut record, RolloutState::Complete)?;
        self.leases.release_all(&record.id)?;
        Ok(())
    }

    async fn bake(
        &self,
        record: RolloutRecord,
        n: usize,
        since: DateTime<Utc>,
    ) -> Result<(), RolloutError> {
        let step = Self::step_of(&record, n)?;
        let slot = record
            .slot
            .clone()
            .ok_or_else(|| RolloutError::NoSlot(record.id.clone()))?;
        let comparator = match step.kind {
            StepKind::Shadow { .. } => Some(ShadowComparator::from_template(
                record
                    .plan
                    .shadow
                    .as_ref()
                    .ok_or_else(|| RolloutError::NoShadowConfig(record.id.clone()))?,
            )),
            _ => None,
        };
        let bake_end = since + step.bake_time;
        let hold_end = bake_end + step.min_duration;
        let resumed_at = self.clock.now();
        let ttl = (hold_end - resumed_at).max(Duration::zero()) + self.config.lease_grace;
        match acquire_all(
            &*self.leases,
            &[record.lease_name()?],
            &record.id,
            ttl,
            resumed_at,
        ) {
            Ok(_) => {}
            Err(LeaseError::Held { name, by, .. }) => {
                let summary = format!(
                    "step {n}: deploy lease {name} is held by {by}, not by this rollout; the split is held for a human"
                );
                return self.halt_and_escalate(record, n, summary, None).await;
            }
            Err(e) => {
                let summary = format!("step {n}: lease store failed ({e})");
                return Err(self.stop_with(&record, n, summary, e.into()).await);
            }
        }
        let mut observed_pairs = 0usize;
        loop {
            let now = self.clock.now();
            if now >= bake_end {
                break;
            }
            match self.check(&record, &slot, n).await {
                Verdict::Healthy => {}
                Verdict::Breach(breach) => return self.on_breach(record, breach).await,
                Verdict::Unavailable(error) => {
                    return self.health_unavailable(record, n, error).await
                }
            }
            if let Some(comparator) = &comparator {
                let samples = match self.shadow.sample(&slot, self.config.poll_interval).await {
                    Ok(samples) => samples,
                    Err(error) => {
                        tracing::warn!(rollout = %record.id, step = n, %error, "Shadow source unavailable; halting");
                        let summary = format!(
                            "step {n}: shadow source unavailable ({error}); the split is held for a human"
                        );
                        return self.halt_and_escalate(record, n, summary, None).await;
                    }
                };
                observed_pairs += samples.len();
                if let Some(breach) = comparator.check(&samples, n) {
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
        if comparator.is_some() && observed_pairs == 0 {
            let summary = "shadow bake observed no mirrored pairs; refusing to promote unobserved"
                .to_string();
            return self.halt_and_escalate(record, n, summary, None).await;
        }
        let now = self.clock.now();
        if now < hold_end {
            self.clock.sleep(hold_end - now).await;
            if !self.unchanged(&record)? {
                return Ok(());
            }
        }
        if let StepKind::Shadow { .. } = step.kind {
            self.guard(&record)?;
            self.adapter.mirror(&slot, 0).await?;
        }
        self.advance_or_complete(record, n).await
    }

    async fn check(&self, record: &RolloutRecord, slot: &Slot, n: usize) -> Verdict {
        let Some(health) = &record.plan.health else {
            return Verdict::Healthy;
        };
        match self.health.sample(slot, self.config.poll_interval).await {
            Ok(sample) => {
                check_health(health, &sample, n).map_or(Verdict::Healthy, Verdict::Breach)
            }
            Err(error) => Verdict::Unavailable(error),
        }
    }

    async fn health_unavailable(
        &self,
        record: RolloutRecord,
        n: usize,
        error: HealthError,
    ) -> Result<(), RolloutError> {
        tracing::warn!(rollout = %record.id, step = n, %error, "Health source unavailable; halting");
        let summary =
            format!("step {n}: health source unavailable ({error}); the split is held for a human");
        self.halt_and_escalate(record, n, summary, None).await
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
            OnBreach::HaltAndEscalate => match &self.incident_responder {
                Some(responder) => {
                    self.respond_to_incident(record, breach, step, responder.clone())
                        .await
                }
                None => {
                    let summary = breach.to_string();
                    self.halt_and_escalate(record, step, summary, Some(breach))
                        .await
                }
            },
        }
    }

    /// Give `responder` a chance to remediate `breach` before it reaches a
    /// human: propose an action from the breach's own evidence, have the
    /// audit hook review it, and either apply an approved rollback or fall
    /// back to halting and escalating (with the proposal in the summary)
    /// for anything else, exactly as `halt-and-escalate` already does
    /// without a responder configured.
    ///
    /// This deliberately does not poll the health source again for a
    /// fresher sample: `breach` already carries the evidence from the
    /// caller's own poll a moment ago, and a fallible poll here, ahead of
    /// any persist, would leave the record stuck `Baking` with live
    /// traffic on a bad slot if the health source failed at exactly this
    /// moment — the one case a responder-less halt never risks, since it
    /// persists `Halted` before doing anything else that can fail.
    async fn respond_to_incident(
        &self,
        record: RolloutRecord,
        breach: HealthBreach,
        step: usize,
        responder: Arc<IncidentResponder>,
    ) -> Result<(), RolloutError> {
        let incident = Incident {
            breach: breach.clone(),
            evidence: breach.evidence.clone(),
            step,
            deploy_id: record.id.clone(),
        };
        let action = responder.propose(&breach, None);
        if let Err(denied) = self.audit.review_action(&incident, &action).await {
            let summary = format!("incident action denied: {}; {breach}", denied.reason);
            return self
                .halt_and_escalate(record, step, summary, Some(breach))
                .await;
        }
        if action == ProposedAction::Rollback && responder.permits(&action) {
            return self
                .roll_back_and_report(record, incident, action, breach)
                .await;
        }
        let summary =
            format!("{breach}; incident responder proposes to {action}: escalating for a human");
        self.halt_and_escalate(record, step, summary, Some(breach))
            .await
    }

    async fn roll_back_and_report(
        &self,
        mut record: RolloutRecord,
        incident: Incident,
        action: ProposedAction,
        breach: HealthBreach,
    ) -> Result<(), RolloutError> {
        let id = record.id.clone();
        let step = incident.step;
        let environment = record.plan.environment.clone();
        let image = record.image.clone();
        let previous_image = record.previous_image.clone();
        self.persist(&mut record, RolloutState::RollingBack)?;
        let outcome = self.roll_back(record).await;
        let (verdict, summary) = match &outcome {
            Ok(()) => (
                "rolled back automatically".to_string(),
                format!("{breach}; incident responder rolled back automatically"),
            ),
            Err(error) => (
                format!("automatic rollback failed: {error}"),
                format!(
                    "{breach}; incident responder's rollback failed ({error}): a human must finish it"
                ),
            ),
        };
        let current = self.log.load(&id)?;
        let postmortem = incident.postmortem(&action, &self.log.history(&id)?, &verdict);
        let escalation = RolloutEscalation {
            rollout_id: id,
            environment,
            image,
            previous_image,
            step,
            traffic_percent: current.traffic_percent,
            summary,
            breach: Some(breach),
            postmortem: Some(postmortem),
        };
        let escalated = self
            .escalation
            .escalate(&escalation)
            .await
            .map_err(RolloutError::Escalation);
        outcome.and(escalated)
    }

    async fn roll_back(&self, mut record: RolloutRecord) -> Result<(), RolloutError> {
        if let Some(retained) = &record.retained_slot {
            self.guard(&record)?;
            self.adapter.set_traffic(retained, 100).await?;
        }
        if let Some(slot) = &record.slot {
            self.guard(&record)?;
            self.adapter.set_traffic(slot, 0).await?;
            self.guard(&record)?;
            self.adapter.mirror(slot, 0).await?;
            self.guard(&record)?;
            self.adapter.clear_fallback(slot).await?;
        }
        self.guard(&record)?;
        self.adapter.rollback_to(&record.previous_image).await?;
        record.set_traffic(0)?;
        record.retained_slot = None;
        record.retained_since = None;
        self.persist(&mut record, RolloutState::RolledBack)?;
        self.leases.release_all(&record.id)?;
        Ok(())
    }

    fn production_hold_for(
        &self,
        record: &RolloutRecord,
    ) -> Option<crate::escalation::IncidentHold> {
        let (log, repo) = self.production_hold.as_ref()?;
        if !crate::deploy::is_production_env(&record.plan.environment) {
            return None;
        }
        log.production_hold(repo.as_deref())
    }

    async fn stop_with(
        &self,
        record: &RolloutRecord,
        step: usize,
        summary: String,
        error: RolloutError,
    ) -> RolloutError {
        let escalation = RolloutEscalation {
            rollout_id: record.id.clone(),
            environment: record.plan.environment.clone(),
            image: record.image.clone(),
            previous_image: record.previous_image.clone(),
            step,
            traffic_percent: record.traffic_percent,
            summary,
            breach: record.breach.clone(),
            postmortem: None,
        };
        if let Err(failure) = self.escalation.escalate(&escalation).await {
            tracing::error!(rollout = %record.id, %failure, "Escalation of a stopped rollout failed");
        }
        error
    }

    async fn halt_and_escalate(
        &self,
        mut record: RolloutRecord,
        step: usize,
        summary: String,
        breach: Option<HealthBreach>,
    ) -> Result<(), RolloutError> {
        self.persist(&mut record, RolloutState::Halted)?;
        self.settle_lease(&record)?;
        let escalation = RolloutEscalation {
            rollout_id: record.id.clone(),
            environment: record.plan.environment.clone(),
            image: record.image.clone(),
            previous_image: record.previous_image.clone(),
            step,
            traffic_percent: record.traffic_percent,
            summary,
            breach,
            postmortem: None,
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
    use crate::deploy::{DeployTemplate, ShadowCompare};
    use crate::leases::{LeaseName, SimulatedClock};
    use crate::rollout::adapter::{AdapterCall, AdapterOp, FallbackPolicy, FallbackSupport, Slot};
    use crate::rollout::health::{HealthError, HealthObservation, HealthSample, HealthThreshold};
    use crate::rollout::hooks::{RecordingAudit, RecordingEscalation};
    use crate::rollout::incident::{IncidentIdentity, IncidentResponder, ProposedAction};
    use crate::rollout::shadow::{FakeShadowSource, ShadowSample};
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
        shadow: Arc<FakeShadowSource>,
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
        let shadow = Arc::new(FakeShadowSource::agreeing(1));
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
        .with_shadow_source(shadow.clone())
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
            shadow,
            audit,
            escalation,
            executor,
        }
    }

    fn overwrite(rig: &Rig, crafted: &RolloutRecord) {
        let mut crafted = crafted.clone();
        let current = rig.executor.log().load(&crafted.id).unwrap();
        crafted.revision = current.revision;
        rig.executor
            .log()
            .append(Some(&current.state), &mut crafted)
            .unwrap();
    }

    fn rig_with_responder(identity: IncidentIdentity) -> Rig {
        let mut rig = rig();
        rig.executor = rig
            .executor
            .with_incident_responder(Arc::new(IncidentResponder::new(identity)));
        rig
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
        let policy = FallbackPolicy::default();
        assert_eq!(
            rig.adapter.calls(),
            vec![
                AdapterCall::CurrentImage,
                AdapterCall::DeployInactive(V2.into()),
                AdapterCall::SetFallback(slot.clone(), policy),
                AdapterCall::SetTraffic(slot.clone(), 10),
                AdapterCall::SetTraffic(slot.clone(), 50),
                AdapterCall::SetTraffic(slot.clone(), 100),
                AdapterCall::ClearFallback(slot.clone()),
            ]
        );
        assert_eq!(done.fallback, Some(FallbackSupport::Native));
        assert_eq!(
            states(&rig.executor, &record.id),
            ["pending", "step", "step", "baking", "step", "baking", "step", "baking", "complete"]
        );
        assert_eq!(rig.health.calls().len(), 6);
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
        rig.health.push_after(5, breach_sample());
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
        assert_eq!(
            calls[calls.len() - 4],
            AdapterCall::SetTraffic(slot.clone(), 0)
        );
        assert_eq!(calls[calls.len() - 3], AdapterCall::Mirror(slot.clone(), 0));
        assert_eq!(calls[calls.len() - 2], AdapterCall::ClearFallback(slot));
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
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let hanging = Arc::new(HangAfter {
            healthy_polls: 1,
            polls: std::sync::Mutex::new(0),
        });
        let crashing = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            hanging,
        )
        .with_config(one_poll_per_step());
        let crashed_run = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            crashing.run(&record.id),
        )
        .await;
        assert!(crashed_run.is_err());
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
                &LeaseName::deploy("registry.example.invalid/ns/app", "staging"),
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
        rig.health.push_after(3, breach_sample());
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
            "a halted rollout with a live split keeps the deploy lease"
        );
        assert_eq!(rig.executor.run(&record.id).await.unwrap(), halted);
    }

    #[tokio::test]
    async fn an_unreviewed_responder_rollback_is_refused_by_default() {
        let rig = rig();
        let executor = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::parse(WINDOWS).unwrap(),
            rig.clock.clone(),
            rig.adapter.clone(),
            rig.health.clone(),
        )
        .with_escalation(rig.escalation.clone())
        .with_incident_responder(Arc::new(IncidentResponder::new(
            IncidentIdentity::incident_responder(),
        )))
        .with_config(one_poll_per_step());
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = executor.start(plan, V2).await.unwrap();
        let done = executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Halted);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::RollbackTo(_))));
        assert!(rig.escalation.escalations()[0]
            .summary
            .contains("incident action denied"));
    }

    #[tokio::test]
    async fn incident_responder_rolls_back_an_approved_action_instead_of_halting() {
        let rig = rig_with_responder(IncidentIdentity::incident_responder());
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        assert_eq!(rig.adapter.current(), V1);
        let escalations = rig.escalation.escalations();
        assert_eq!(
            escalations.len(),
            1,
            "an automatic rollback must be reported"
        );
        assert!(escalations[0].summary.contains("rolled back automatically"));
        assert_eq!(escalations[0].rollout_id, record.id);
        assert_eq!(escalations[0].traffic_percent, 0);
        let postmortem = escalations[0].postmortem.as_ref().expect("postmortem");
        assert!(postmortem.title.contains(&record.id));
        assert!(postmortem.body.contains("rolled back automatically"));
        assert!(postmortem.body.contains("rolled-back"));
        assert_eq!(
            rig.leases.snapshot().unwrap().len(),
            0,
            "a completed rollback releases the deploy lease"
        );
    }

    #[tokio::test]
    async fn a_failed_responder_rollback_is_still_escalated_with_a_postmortem() {
        let rig = rig_with_responder(IncidentIdentity::incident_responder());
        rig.adapter.fail(AdapterOp::RollbackTo);
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert!(matches!(err, RolloutError::Adapter(_)), "{err}");
        assert_eq!(
            rig.executor.status(&record.id).unwrap().state,
            RolloutState::RollingBack
        );
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("rollback failed"));
        assert!(escalations[0]
            .postmortem
            .as_ref()
            .unwrap()
            .body
            .contains("automatic rollback failed"));
    }

    #[tokio::test]
    async fn responder_actions_that_escalate_carry_no_postmortem_and_a_denied_one_neither() {
        let rig = rig_with_responder(IncidentIdentity::incident_responder());
        rig.audit.deny_next_action("no");
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        rig.executor.run(&record.id).await.unwrap();
        assert_eq!(rig.escalation.escalations()[0].postmortem, None);
    }

    #[tokio::test]
    async fn incident_responder_action_denied_by_audit_halts_and_escalates() {
        let rig = rig_with_responder(IncidentIdentity::incident_responder());
        rig.audit.deny_next_action("evidence is inconclusive");
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0]
            .summary
            .contains("incident action denied: evidence is inconclusive"));
        assert_eq!(
            rig.audit.action_reviews(),
            vec![(record.id.clone(), ProposedAction::Rollback)]
        );
    }

    #[tokio::test]
    async fn incident_responder_without_the_rollback_tool_escalates_instead_of_acting() {
        let identity = IncidentIdentity::from_toml_str(
            "[identity]\nname = \"x\"\n[scope]\nrepos = []\npaths = []\nmax_effect = \"production\"\ntools = [\"read_logs\"]\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n",
        )
        .unwrap();
        let rig = rig_with_responder(identity);
        rig.health.push_after(1, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(rig.adapter.current(), V1, "no rollback was applied");
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0]
            .summary
            .contains("incident responder proposes to rollback: escalating for a human"));
    }

    #[tokio::test]
    async fn manual_rollback_policy_halts_on_breach() {
        let rig = rig();
        rig.health.push_after(1, breach_sample());
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
        assert_eq!(forwarded.fallback, None);
        let old = Slot::new("slot-1");
        assert!(rig.adapter.calls().ends_with(&[
            AdapterCall::SetTraffic(old.clone(), 0),
            AdapterCall::Mirror(old.clone(), 0),
            AdapterCall::ClearFallback(old.clone()),
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

    struct FailAfter {
        healthy_polls: usize,
        polls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl HealthSource for FailAfter {
        async fn sample(
            &self,
            _slot: &Slot,
            _window: Duration,
        ) -> Result<HealthSample, HealthError> {
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            if *polls > self.healthy_polls {
                return Err(HealthError("scripted failure".into()));
            }
            Ok(HealthSample::healthy(&["/health/v1".to_string()]))
        }
    }

    struct HangAfter {
        healthy_polls: usize,
        polls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl HealthSource for HangAfter {
        async fn sample(
            &self,
            _slot: &Slot,
            _window: Duration,
        ) -> Result<HealthSample, HealthError> {
            let hang = {
                let mut polls = self.polls.lock().unwrap();
                *polls += 1;
                *polls > self.healthy_polls
            };
            if hang {
                std::future::pending::<()>().await;
            }
            Ok(HealthSample::healthy(&["/health/v1".to_string()]))
        }
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
                self.log.append(Some(&record.state), &mut halted).unwrap();
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
            after: 3,
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

    #[tokio::test]
    async fn operator_halt_during_a_step_is_not_overwritten_by_the_step_result() {
        let rig = rig();
        let log = RolloutLog::open(&rig.path).unwrap();
        let health = Arc::new(HaltOnPoll {
            log: log.clone(),
            after: 1,
            polls: std::sync::Mutex::new(0),
        });
        let executor = RolloutExecutor::new(
            log.clone(),
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
        let outcome = executor.run(&record.id).await.unwrap();
        assert_eq!(outcome.state, RolloutState::Halted);
        let history = log.history(&record.id).unwrap();
        let last = history.last().unwrap();
        assert_eq!(last.record.state, RolloutState::Halted);
        assert!(
            history
                .iter()
                .skip_while(|t| t.record.state != RolloutState::Halted)
                .all(|t| t.record.state == RolloutState::Halted),
            "nothing may follow the halt: {history:?}"
        );
    }

    #[tokio::test]
    async fn a_halt_at_the_health_poll_stops_the_adapter_from_receiving_set_traffic() {
        let rig = rig();
        let log = RolloutLog::open(&rig.path).unwrap();
        let health = Arc::new(HaltOnPoll {
            log: log.clone(),
            after: 1,
            polls: std::sync::Mutex::new(0),
        });
        let executor = RolloutExecutor::new(
            log.clone(),
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
        let outcome = executor.run(&record.id).await.unwrap();
        assert_eq!(outcome.state, RolloutState::Halted);
        let calls = rig.adapter.calls();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, AdapterCall::SetTraffic(..))),
            "no traffic may move after a halt: {calls:?}"
        );
        assert_eq!(rig.adapter.traffic(&Slot::new("slot-1")), Some(0));
    }

    struct HaltAndRollForwardOnPoll {
        log: RolloutLog,
        polls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl HealthSource for HaltAndRollForwardOnPoll {
        async fn sample(
            &self,
            _slot: &Slot,
            _window: Duration,
        ) -> Result<HealthSample, HealthError> {
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            if *polls == 1 {
                let record = self.log.latest().unwrap().into_values().next().unwrap();
                let mut halted = record.clone();
                halted
                    .transition(RolloutState::Halted, record.updated_at)
                    .unwrap();
                self.log.append(Some(&record.state), &mut halted).unwrap();
                let mut forward = halted.clone();
                forward
                    .transition(RolloutState::Step(0), record.updated_at)
                    .unwrap();
                forward.image = V3.to_string();
                forward.slot = None;
                forward.traffic_percent = 0;
                self.log.append(Some(&halted.state), &mut forward).unwrap();
            }
            Ok(HealthSample::healthy(&["/health/v1".to_string()]))
        }
    }

    #[tokio::test]
    async fn a_stale_executor_cannot_drive_the_old_image_after_halt_and_roll_forward() {
        let rig = rig();
        let log = RolloutLog::open(&rig.path).unwrap();
        let health = Arc::new(HaltAndRollForwardOnPoll {
            log: log.clone(),
            polls: std::sync::Mutex::new(0),
        });
        let executor = RolloutExecutor::new(
            log.clone(),
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
        let _ = executor.run(&record.id).await;
        let history = log.history(&record.id).unwrap();
        assert!(
            history
                .iter()
                .skip_while(|t| t.record.image != V3)
                .all(|t| t.record.image == V3),
            "the stale executor wrote the old image after the roll-forward: {history:?}"
        );
        assert!(
            !rig.adapter.calls().iter().any(
                |c| matches!(c, AdapterCall::SetTraffic(slot, _) if *slot == Slot::new("slot-1"))
            ),
            "{:?}",
            rig.adapter.calls()
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
                self.log.append(Some(&record.state), &mut halted).unwrap();
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
    async fn best_effort_fallback_is_recorded_and_shown_in_status() {
        let rig = rig();
        rig.adapter
            .set_fallback_support(FallbackSupport::BestEffort);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.fallback, Some(FallbackSupport::BestEffort));
        assert!(done.summary().ends_with("fallback: best-effort"));
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
    async fn a_refused_set_fallback_leaves_the_deployed_slot_recorded_and_reused() {
        let rig = rig();
        rig.adapter.fail(AdapterOp::SetFallback);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        for _ in 0..3 {
            assert!(matches!(
                rig.executor.run(&record.id).await.unwrap_err(),
                RolloutError::Adapter(_)
            ));
        }
        let stuck = rig.executor.status(&record.id).unwrap();
        assert_eq!(stuck.state, RolloutState::Step(0));
        assert_eq!(stuck.slot, Some(Slot::new("slot-1")));
        assert_eq!(stuck.fallback, None);
        assert_eq!(
            rig.adapter
                .calls()
                .iter()
                .filter(|c| matches!(c, AdapterCall::DeployInactive(_)))
                .count(),
            1,
            "every refusal deployed another untracked slot"
        );
        rig.adapter.succeed(AdapterOp::SetFallback);
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.slot, Some(Slot::new("slot-1")));
        assert_eq!(done.fallback, Some(FallbackSupport::Native));
    }

    #[tokio::test]
    async fn no_traffic_moves_before_the_first_health_sample() {
        let rig = rig();
        rig.health.push(breach_sample());
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.run(&record.id).await.unwrap();
        assert!(
            !rig.adapter
                .calls()
                .iter()
                .any(|c| matches!(c, AdapterCall::SetTraffic(_, 10))),
            "{:?}",
            rig.adapter.calls()
        );
    }

    fn executor_with_health(rig: &Rig, health: Arc<dyn HealthSource>) -> RolloutExecutor {
        RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            health,
        )
        .with_escalation(rig.escalation.clone())
        .with_config(one_poll_per_step())
    }

    #[tokio::test]
    async fn health_ok_precondition_without_thresholds_halts_before_traffic() {
        let rig = rig();
        let mut unthresholded = plan("sandbox");
        unthresholded.health = None;
        assert!(unthresholded.steps[0]
            .preconditions
            .contains(&Precondition::HealthOk));
        let record = rig.executor.start(unthresholded, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 0);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::SetTraffic(..))));
        assert_eq!(rig.escalation.escalations().len(), 1);
    }

    #[tokio::test]
    async fn health_source_error_before_traffic_halts_and_escalates() {
        let rig = rig();
        rig.health.set_failing(true);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 0);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::SetTraffic(..))));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("scripted failure"));
    }

    #[tokio::test]
    async fn health_source_error_mid_bake_halts_holding_the_split() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let failing = Arc::new(FailAfter {
            healthy_polls: 1,
            polls: std::sync::Mutex::new(0),
        });
        let executor = executor_with_health(&rig, failing);
        let halted = executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert_eq!(rig.adapter.traffic(&Slot::new("slot-1")), Some(10));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert_eq!(escalations[0].traffic_percent, 10);
        assert!(escalations[0].summary.contains("scripted failure"));
    }

    #[tokio::test]
    async fn roll_forward_is_persisted_before_adapter_effects() {
        let rig = rig();
        rig.health.push(breach_sample());
        let plan = plan_with("[rollback]\nautomatic = false\non_breach = \"rollback\"\n");
        let record = rig.executor.start(plan, V2).await.unwrap();
        assert_eq!(
            rig.executor.run(&record.id).await.unwrap().state,
            RolloutState::Halted
        );
        rig.adapter.fail(AdapterOp::Retire);
        assert!(rig
            .executor
            .roll_forward(&record.id, V3, Some("pr-1"))
            .await
            .is_err());
        let persisted = rig.executor.status(&record.id).unwrap();
        assert_eq!(persisted.image, V3);
        assert_eq!(persisted.state, RolloutState::Step(0));
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
            .append(Some(&record.state), &mut crafted)
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
            AdapterCall::SetTraffic(slot.clone(), 0),
            AdapterCall::Mirror(slot.clone(), 0),
            AdapterCall::ClearFallback(slot),
            AdapterCall::RollbackTo(V1.into())
        ]));
    }

    #[tokio::test]
    async fn corrupt_records_are_refused_not_guessed() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let mut crafted = record.clone();
        crafted.state = RolloutState::Step(1);
        overwrite(&rig, &crafted);
        assert!(
            matches!(rig.executor.run(&record.id).await.unwrap_err(), RolloutError::NoSlot(id) if id == record.id)
        );
        crafted.state = RolloutState::Step(7);
        overwrite(&rig, &crafted);
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoSuchStep { step: 7, .. }
        ));
        crafted.state = RolloutState::Baking {
            step: 9,
            since: t0(),
        };
        overwrite(&rig, &crafted);
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoSuchStep { step: 9, .. }
        ));
        crafted.state = RolloutState::Baking {
            step: 0,
            since: t0(),
        };
        overwrite(&rig, &crafted);
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
    async fn blue_green_swaps_and_retires_only_after_retain_for() {
        let rig = rig();
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let old = Slot::new("slot-0");
        let new = Slot::new("slot-1");
        let parked = rig.executor.run(&record.id).await.unwrap();
        let until = t0() + Duration::days(2);
        assert_eq!(
            parked.state,
            RolloutState::Parked {
                until,
                resume_state: Box::new(RolloutState::Step(1)),
            },
            "retire waits on the clock rather than blocking it"
        );
        assert_eq!(parked.retained_slot, Some(old.clone()));
        assert_eq!(rig.adapter.image_in(&old), Some(V1.to_string()));
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::Retire(_))));
        assert!(
            rig.leases.snapshot().unwrap().is_empty(),
            "released while parked"
        );
        assert_eq!(
            rig.executor.run(&record.id).await.unwrap(),
            parked,
            "a run before retain_for elapses is a no-op"
        );
        rig.clock.advance(until - rig.clock.now());
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.traffic_percent, 100);
        assert_eq!(done.slot, Some(new.clone()));
        assert_eq!(done.retained_slot, None, "cleared once retired");
        assert_eq!(rig.adapter.current(), V2);
        assert_eq!(rig.adapter.active(), new);
        assert_eq!(rig.adapter.traffic(&new), Some(100));
        assert_eq!(
            rig.adapter.image_in(&old),
            None,
            "retired once retain_for elapsed"
        );
        assert_eq!(
            rig.adapter.calls(),
            vec![
                AdapterCall::CurrentImage,
                AdapterCall::DeployInactive(V2.into()),
                AdapterCall::SetFallback(new.clone(), FallbackPolicy::default()),
                AdapterCall::Swap,
                AdapterCall::ClearFallback(old.clone()),
                AdapterCall::Retire(old.clone()),
                AdapterCall::ClearFallback(new.clone()),
            ]
        );
        assert_eq!(done.fallback, Some(FallbackSupport::Native));
        assert_eq!(
            states(&rig.executor, &record.id),
            ["pending", "step", "step", "baking", "step", "parked", "step", "complete"]
        );
        assert_eq!(rig.clock.now(), until);
        assert!(rig.leases.snapshot().unwrap().is_empty());
        assert!(rig.escalation.escalations().is_empty());
    }

    #[tokio::test]
    async fn run_simulated_drives_blue_green_through_the_park_to_completion() {
        let rig = rig();
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let start = rig.clock.now();
        let steps = run_simulated(&rig.executor, &rig.clock, &record.id)
            .await
            .unwrap();
        let states: Vec<&'static str> = steps.iter().map(|r| r.state.name()).collect();
        assert_eq!(states, ["parked", "complete"]);
        assert_eq!(steps[0].retained_slot, Some(Slot::new("slot-0")));
        assert_eq!(steps[1].retained_slot, None);
        assert_eq!(
            rig.clock.now(),
            start + Duration::days(2),
            "retain_for is measured from the swap, not from the swap step's own hold"
        );
        let history = rig.executor.log().history(&record.id).unwrap();
        assert!(
            history
                .iter()
                .any(|t| t.summary().contains("retained: slot-0")),
            "{history:#?}"
        );
    }

    #[tokio::test]
    async fn rollback_after_swap_restores_the_retained_slot() {
        let rig = rig();
        rig.health.push_after(1, breach_sample());
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        assert_eq!(done.breach.as_ref().unwrap().step, 0);
        let old = Slot::new("slot-0");
        let new = Slot::new("slot-1");
        assert_eq!(done.retained_slot, None, "cleared on rollback");
        assert_eq!(rig.adapter.current(), V1);
        assert_eq!(rig.adapter.active(), old);
        assert_eq!(rig.adapter.traffic(&old), Some(100));
        assert_eq!(rig.adapter.traffic(&new), Some(0));
        assert_eq!(rig.adapter.image_in(&old).as_deref(), Some(V1));
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::Retire(_))));
        let calls = rig.adapter.calls();
        let restore = calls
            .iter()
            .position(|c| *c == AdapterCall::SetTraffic(old.clone(), 100))
            .unwrap();
        let drain = calls
            .iter()
            .position(|c| *c == AdapterCall::SetTraffic(new.clone(), 0))
            .unwrap();
        assert!(
            restore < drain,
            "the retained slot is restored before the bad one is drained"
        );
    }

    #[tokio::test]
    async fn roll_forward_after_a_swap_restores_the_retained_slot_before_draining_the_bad_one() {
        let rig = rig();
        rig.health.push_after(1, breach_sample());
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[rollback]\nautomatic = true\non_breach = \"roll-forward\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        let old = Slot::new("slot-0");
        let bad = Slot::new("slot-1");
        assert_eq!(halted.slot, Some(bad.clone()));
        assert_eq!(halted.retained_slot, Some(old.clone()));
        assert_eq!(
            rig.adapter.current(),
            V2,
            "the swap already happened before the breach was detected"
        );
        let forwarded = rig
            .executor
            .roll_forward(
                &record.id,
                V3,
                Some("https://github.com/example/repo/pull/9"),
            )
            .await
            .unwrap();
        assert_eq!(forwarded.state, RolloutState::Step(0));
        assert_eq!(forwarded.retained_slot, None);
        assert_eq!(forwarded.slot, None);
        assert_eq!(
            rig.adapter.active(),
            old,
            "the retained slot serves live traffic again"
        );
        assert_eq!(rig.adapter.current(), V1);
        assert_eq!(rig.adapter.traffic(&old), Some(100));
        assert_eq!(rig.adapter.image_in(&bad), None, "the bad slot is retired");
        let calls = rig.adapter.calls();
        let restore = calls
            .iter()
            .position(|c| *c == AdapterCall::SetTraffic(old.clone(), 100))
            .unwrap();
        let retire = calls
            .iter()
            .position(|c| *c == AdapterCall::Retire(bad.clone()))
            .unwrap();
        assert!(
            restore < retire,
            "the retained slot is restored before the bad one is retired"
        );
    }

    const SHADOW_THEN_GRADUAL: &str ="[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"shadow-then-gradual\"\nsteps = [50, 100]\nmin_step_duration = \"1h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[shadow]\nenabled = true\nmirror_percent = 15\ncompare = [\"status\", \"latency\"]\nmax_divergence = 0.2\n[rollback]\nautomatic = true\non_breach = \"rollback\"\n";

    fn shadow_plan() -> DeployPlan {
        DeployTemplate::parse(SHADOW_THEN_GRADUAL)
            .unwrap()
            .plan("sandbox")
            .unwrap()
    }

    #[tokio::test]
    async fn shadow_then_gradual_mirrors_then_promotes_to_full_traffic() {
        let rig = rig();
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert_eq!(done.traffic_percent, 100);
        let slot = Slot::new("slot-1");
        assert_eq!(done.slot, Some(slot.clone()));
        assert_eq!(rig.adapter.current(), V2);
        assert_eq!(rig.adapter.active(), slot, "the 100% step cuts over live");
        assert_eq!(
            rig.adapter.mirrored(&slot),
            Some(0),
            "cleared once the shadow step finished"
        );
        assert_eq!(rig.adapter.max_traffic(&slot), Some(100));
        assert_eq!(
            rig.adapter.calls(),
            vec![
                AdapterCall::CurrentImage,
                AdapterCall::DeployInactive(V2.into()),
                AdapterCall::SetFallback(slot.clone(), FallbackPolicy::default()),
                AdapterCall::Mirror(slot.clone(), 15),
                AdapterCall::Mirror(slot.clone(), 0),
                AdapterCall::SetTraffic(slot.clone(), 50),
                AdapterCall::SetTraffic(slot.clone(), 100),
                AdapterCall::ClearFallback(slot.clone()),
            ]
        );
        assert_eq!(done.fallback, Some(FallbackSupport::Native));
        assert_eq!(
            states(&rig.executor, &record.id),
            ["pending", "step", "step", "baking", "step", "baking", "step", "baking", "complete"]
        );
        assert_eq!(rig.health.calls().len(), 6);
        assert_eq!(
            rig.shadow.calls().len(),
            1,
            "only the shadow step polls the comparator"
        );
        assert_eq!(
            rig.clock.now(),
            t0() + Duration::minutes(10) * 3 + Duration::hours(1) * 3
        );
    }

    #[tokio::test]
    async fn an_unhealthy_source_before_the_shadow_step_halts_without_mirroring() {
        let rig = rig();
        rig.health.push(breach_sample());
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        assert_eq!(done.breach.as_ref().unwrap().step, 0);
        assert!(
            !rig.adapter
                .calls()
                .iter()
                .any(|c| matches!(c, AdapterCall::Mirror(_, percent) if *percent > 0)),
            "{:?}",
            rig.adapter.calls()
        );
        assert_eq!(rig.shadow.calls().len(), 0);
    }

    #[tokio::test]
    async fn a_failing_health_source_before_the_shadow_step_halts_and_escalates_without_mirroring()
    {
        let rig = rig();
        rig.health.set_failing(true);
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Halted);
        assert_eq!(rig.escalation.escalations().len(), 1);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::Mirror(_, percent) if *percent > 0)));
    }

    #[tokio::test]
    async fn an_unhealthy_source_before_retire_leaves_the_retained_slot_in_place() {
        let rig = rig();
        rig.health.push_after(2, breach_sample());
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V2).await.unwrap();
        let parked = rig.executor.run(&record.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        rig.clock.advance(Duration::days(2));
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        assert_eq!(done.breach.as_ref().unwrap().step, 1);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::Retire(_))));
    }

    #[tokio::test]
    async fn shadow_divergence_above_the_threshold_is_a_breach_that_rolls_back() {
        let rig = rig();
        let diverging = vec![
            ShadowSample {
                status_active: 200,
                status_shadow: 500,
                latency_active_ms: 10,
                latency_shadow_ms: 10,
            };
            4
        ];
        rig.shadow.push(diverging);
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::RolledBack);
        let breach = done.breach.unwrap();
        assert_eq!(breach.step, 0);
        assert_eq!(
            breach.threshold,
            HealthThreshold::ShadowDivergenceMax {
                compare: ShadowCompare::Status,
                max: 0.2
            }
        );
        assert_eq!(breach.observed, HealthObservation::ShadowDivergence(1.0));
        assert_eq!(rig.adapter.current(), V1);
        assert_eq!(rig.adapter.traffic(&Slot::new("slot-0")), Some(100));
    }

    #[tokio::test]
    async fn an_unobserved_shadow_window_refuses_to_promote() {
        let rig = rig();
        let executor = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            rig.health.clone(),
        )
        .with_shadow_source(Arc::new(FakeShadowSource::new(Vec::new())))
        .with_escalation(rig.escalation.clone())
        .with_config(one_poll_per_step());
        let record = executor.start(shadow_plan(), V2).await.unwrap();
        let done = executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Halted);
        assert!(!rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::SetTraffic(_, 50 | 100))));
        assert!(rig.escalation.escalations()[0]
            .summary
            .contains("unobserved"));
    }

    #[tokio::test]
    async fn corrupt_shadow_and_retire_records_are_refused_not_guessed() {
        let rig = rig();
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let mut crafted = record.clone();
        crafted.slot = Some(Slot::new("slot-1"));
        crafted.state = RolloutState::Baking {
            step: 0,
            since: t0(),
        };
        crafted.plan.shadow = None;
        overwrite(&rig, &crafted);
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoShadowConfig(id) if id == record.id
        ));

        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        let record = rig.executor.start(plan, V3).await.unwrap();
        let mut crafted = record.clone();
        crafted.slot = Some(Slot::new("slot-2"));
        crafted.retained_slot = None;
        crafted.state = RolloutState::Step(1);
        overwrite(&rig, &crafted);
        assert!(matches!(
            rig.executor.run(&record.id).await.unwrap_err(),
            RolloutError::NoRetainedSlot(id) if id == record.id
        ));
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

    fn held_by(rig: &Rig) -> Vec<String> {
        rig.leases
            .snapshot()
            .unwrap()
            .into_iter()
            .map(|l| l.holder)
            .collect()
    }

    struct LeaseProbe {
        leases: Arc<InMemoryLeaseStore>,
        clock: Arc<SimulatedClock>,
        seen: std::sync::Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl HealthSource for LeaseProbe {
        async fn sample(
            &self,
            _slot: &Slot,
            _window: Duration,
        ) -> Result<HealthSample, HealthError> {
            let now = self.clock.now();
            let holders = self
                .leases
                .snapshot()
                .unwrap()
                .into_iter()
                .filter(|l| !l.is_expired(now))
                .map(|l| l.holder)
                .collect();
            self.seen.lock().unwrap().push(holders);
            Ok(HealthSample::healthy(&["/health/v1".to_string()]))
        }
    }

    #[tokio::test]
    async fn the_lease_is_held_before_the_first_effect_and_throughout_the_rollout() {
        let rig = rig();
        let probe = Arc::new(LeaseProbe {
            leases: rig.leases.clone(),
            clock: rig.clock.clone(),
            seen: std::sync::Mutex::default(),
        });
        let executor = executor_with_health(&rig, probe.clone());
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        let done = executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        let seen = probe.seen.lock().unwrap().clone();
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|holders| *holders == [record.id.clone()]),
            "{seen:?}"
        );
        assert!(held_by(&rig).is_empty());
    }

    #[tokio::test]
    async fn a_lease_precondition_naming_another_lease_is_refused() {
        let rig = rig();
        let mut forged = plan("sandbox");
        for step in &mut forged.steps {
            step.preconditions[0] = Precondition::LeaseHeld("deploy:other/app:sandbox".into());
        }
        let record = rig.executor.start(forged, V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 0);
        assert!(!rig.adapter.calls().iter().any(|c| matches!(
            c,
            AdapterCall::SetTraffic(..) | AdapterCall::DeployInactive(..)
        )));
        assert_eq!(rig.escalation.escalations().len(), 1);
        assert!(rig.escalation.escalations()[0]
            .summary
            .contains("deploy:other/app:sandbox"));
        assert!(held_by(&rig).is_empty());
    }

    fn set_traffic_calls(rig: &Rig) -> usize {
        rig.adapter
            .calls()
            .iter()
            .filter(|c| matches!(c, AdapterCall::SetTraffic(..)))
            .count()
    }

    #[tokio::test]
    async fn a_halt_with_zero_traffic_releases_the_lease() {
        let rig = rig();
        rig.audit.deny_from(0, "no");
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 0);
        assert!(held_by(&rig).is_empty());

        let rig = self::rig();
        rig.health.set_failing(true);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.traffic_percent, 0);
        assert!(held_by(&rig).is_empty());
    }

    #[tokio::test]
    async fn a_halt_with_zero_traffic_releases_the_lease_even_when_escalation_fails() {
        let rig = rig();
        rig.escalation.set_failing(true);
        rig.audit.deny_from(0, "no");
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(rig.executor.run(&record.id).await.is_err());
        assert!(held_by(&rig).is_empty());
    }

    #[tokio::test]
    async fn a_halt_with_a_live_split_keeps_the_lease_and_refuses_a_second_rollout() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&first.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(halted.traffic_percent, 10);
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));
        assert_eq!(rig.executor.run(&first.id).await.unwrap(), halted);
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));

        let before = set_traffic_calls(&rig);
        let second = rig.executor.start(plan("sandbox"), V3).await.unwrap();
        let parked = rig.executor.run(&second.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        assert_eq!(parked.traffic_percent, 0);
        assert_eq!(set_traffic_calls(&rig), before);
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));
    }

    #[tokio::test]
    async fn a_halt_with_a_live_split_keeps_the_lease_when_escalation_fails() {
        let rig = rig();
        rig.escalation.set_failing(true);
        rig.audit.deny_from(1, "no");
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(rig.executor.run(&record.id).await.is_err());
        assert_eq!(held_by(&rig), std::slice::from_ref(&record.id));
    }

    #[tokio::test]
    async fn lease_expiry_does_not_free_a_live_split() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.run(&first.id).await.unwrap();
        rig.clock.advance(Duration::days(365));
        let second = rig.executor.start(plan("sandbox"), V3).await.unwrap();
        let parked = rig.executor.run(&second.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        assert_eq!(
            rig.leases.expired(rig.clock.now()).unwrap(),
            vec![],
            "the held lease has not lapsed"
        );
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));
    }

    async fn crashed_baking(rig: &Rig) -> RolloutRecord {
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let crashing = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            Arc::new(HangAfter {
                healthy_polls: 1,
                polls: std::sync::Mutex::new(0),
            }),
        )
        .with_config(one_poll_per_step());
        let run = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            crashing.run(&record.id),
        )
        .await;
        assert!(run.is_err());
        let crashed = rig.executor.log().load(&record.id).unwrap();
        assert!(matches!(crashed.state, RolloutState::Baking { .. }));
        crashed
    }

    #[tokio::test]
    async fn a_rollout_resumed_in_baking_reacquires_an_absent_lease_before_sampling() {
        let rig = rig();
        let crashed = crashed_baking(&rig).await;
        rig.leases.release_all(&crashed.id).unwrap();
        let probe = Arc::new(LeaseProbe {
            leases: rig.leases.clone(),
            clock: rig.clock.clone(),
            seen: std::sync::Mutex::default(),
        });
        let resumed = executor_with_health(&rig, probe.clone());
        let done = resumed.run(&crashed.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        let seen = probe.seen.lock().unwrap().clone();
        assert!(!seen.is_empty());
        assert!(
            seen.iter().all(|holders| *holders == [crashed.id.clone()]),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn a_rollout_resumed_in_baking_reclaims_an_expired_lease() {
        let rig = rig();
        let crashed = crashed_baking(&rig).await;
        rig.leases.release_all(&crashed.id).unwrap();
        rig.leases
            .acquire(
                &crashed.lease_name().unwrap(),
                &crashed.id,
                Duration::minutes(1),
                rig.clock.now(),
            )
            .unwrap();
        rig.clock.advance(Duration::minutes(10));
        assert!(rig
            .leases
            .snapshot()
            .unwrap()
            .iter()
            .all(|l| l.is_expired(rig.clock.now())));
        let probe = Arc::new(LeaseProbe {
            leases: rig.leases.clone(),
            clock: rig.clock.clone(),
            seen: std::sync::Mutex::default(),
        });
        let resumed = executor_with_health(&rig, probe.clone());
        let done = resumed.run(&crashed.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        let seen = probe.seen.lock().unwrap().clone();
        assert!(!seen.is_empty());
        for holders in seen {
            assert_eq!(holders, std::slice::from_ref(&crashed.id));
        }
    }

    #[tokio::test]
    async fn a_lease_store_failure_when_resuming_a_bake_is_escalated() {
        let rig = rig();
        let crashed = crashed_baking(&rig).await;
        let executor = executor_with_health(&rig, rig.health.clone()).with_config(RolloutConfig {
            poll_interval: Duration::minutes(30),
            lease_grace: Duration::hours(-100),
        });
        let err = executor.run(&crashed.id).await.unwrap_err();
        assert!(matches!(
            err,
            RolloutError::Lease(LeaseError::NonPositiveTtl(_))
        ));
        assert_eq!(rig.escalation.escalations().len(), 1);
        assert!(rig.escalation.escalations()[0]
            .summary
            .starts_with("step 0: lease store failed"));
    }

    #[tokio::test]
    async fn a_rollout_resumed_in_baking_halts_when_another_holder_owns_the_lease() {
        let rig = rig();
        let crashed = crashed_baking(&rig).await;
        rig.leases.release_all(&crashed.id).unwrap();
        rig.leases
            .acquire(
                &crashed.lease_name().unwrap(),
                "usurper",
                Duration::days(1),
                rig.clock.now(),
            )
            .unwrap();
        let probe = Arc::new(LeaseProbe {
            leases: rig.leases.clone(),
            clock: rig.clock.clone(),
            seen: std::sync::Mutex::default(),
        });
        let resumed = executor_with_health(&rig, probe.clone());
        let halted = resumed.run(&crashed.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert!(probe.seen.lock().unwrap().is_empty());
        assert_eq!(held_by(&rig), ["usurper".to_string()]);
        assert_eq!(rig.escalation.escalations().len(), 1);
        assert!(rig.escalation.escalations()[0].summary.contains("usurper"));
    }

    fn unlisted_env_plan() -> DeployPlan {
        DeployTemplate::parse(
            &crate::rollout::state::tests::GRADUAL.replace("\"production\"", "\"prod\""),
        )
        .unwrap()
        .plan("prod")
        .unwrap()
    }

    fn set_traffic_percents(rig: &Rig) -> Vec<u8> {
        rig.adapter
            .calls()
            .iter()
            .filter_map(|c| match c {
                AdapterCall::SetTraffic(_, percent) => Some(*percent),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_unlisted_environment_name_is_gated_on_the_enforced_path() {
        let plan = unlisted_env_plan();
        assert_eq!(
            plan.steps[0].preconditions,
            [
                Precondition::LeaseHeld(plan.lease.clone()),
                Precondition::WindowOpen("business-hours".into()),
                Precondition::HealthOk,
            ]
        );

        let window = rig();
        let record = window.executor.start(plan.clone(), V2).await.unwrap();
        let parked = window.executor.run(&record.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        assert_eq!(set_traffic_percents(&window), [10]);
        assert!(held_by(&window).is_empty());

        let health = self::rig();
        health.health.set_failing(true);
        let record = health.executor.start(plan.clone(), V2).await.unwrap();
        let halted = health.executor.run(&record.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert!(set_traffic_percents(&health).is_empty());

        let lease = self::rig();
        lease
            .leases
            .acquire(
                &LeaseName::deploy_image("registry.example.invalid/ns", "app", "prod"),
                "other",
                Duration::hours(2),
                t0(),
            )
            .unwrap();
        let record = lease.executor.start(plan, V2).await.unwrap();
        let parked = lease.executor.run(&record.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        assert!(set_traffic_percents(&lease).is_empty());
        assert!(lease
            .adapter
            .calls()
            .iter()
            .all(|c| *c == AdapterCall::CurrentImage));
    }

    #[tokio::test]
    async fn a_live_split_lease_outlives_the_ttl_that_frees_an_ordinary_lease() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let halted = rig.executor.run(&first.id).await.unwrap();
        assert_eq!(halted.traffic_percent, 10);
        let ordinary = LeaseName::deploy("example/ordinary", "sandbox");
        let step = &halted.plan.steps[1];
        let normal_ttl = step.min_duration + step.bake_time + one_poll_per_step().lease_grace;
        let granted_at = rig.clock.now();
        rig.leases
            .acquire(&ordinary, "ordinary", normal_ttl, granted_at)
            .unwrap();
        let held = rig.leases.snapshot().unwrap();
        let kept = held.iter().find(|l| l.holder == first.id).unwrap();
        assert!(kept.until - rig.clock.now() > normal_ttl * 1000);

        for elapsed in [
            normal_ttl + Duration::seconds(1),
            Duration::days(30),
            Duration::days(36_000),
        ] {
            let target = granted_at + elapsed;
            rig.clock.advance(target - rig.clock.now());
            let lapsed: Vec<_> = rig
                .leases
                .snapshot()
                .unwrap()
                .into_iter()
                .filter(|l| l.is_expired(rig.clock.now()))
                .map(|l| l.holder)
                .collect();
            assert_eq!(lapsed, ["ordinary".to_string()], "after {elapsed}");
            let second = rig.executor.start(plan("sandbox"), V3).await.unwrap();
            let parked = rig.executor.run(&second.id).await.unwrap();
            let RolloutState::Parked { until, .. } = parked.state else {
                panic!("second rollout was not refused after {elapsed}: {parked:?}");
            };
            assert!(until > rig.clock.now());
            assert_eq!(parked.traffic_percent, 0);
            assert!(rig
                .leases
                .snapshot()
                .unwrap()
                .iter()
                .any(|l| l.holder == first.id));
            rig.leases.release_all(&second.id).unwrap();
        }
    }

    #[tokio::test]
    async fn rollback_releases_the_lease_and_a_second_rollout_starts() {
        let rig = rig();
        rig.health.push_after(3, breach_sample());
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        assert_eq!(
            rig.executor.run(&first.id).await.unwrap().state,
            RolloutState::RolledBack
        );
        assert!(held_by(&rig).is_empty());
        let second = rig.executor.start(plan("sandbox"), V3).await.unwrap();
        assert_eq!(
            rig.executor.run(&second.id).await.unwrap().state,
            RolloutState::Complete
        );
    }

    #[tokio::test]
    async fn roll_forward_from_a_held_split_is_the_human_release_path() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.run(&first.id).await.unwrap();
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));
        rig.audit.deny_from(99, "reset");
        rig.executor
            .roll_forward(&first.id, V3, Some("https://example.invalid/pr/1"))
            .await
            .unwrap();
        let done = rig.executor.run(&first.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Complete);
        assert!(held_by(&rig).is_empty());
        let second = rig.executor.start(plan("sandbox"), V3).await.unwrap();
        assert_eq!(
            rig.executor.run(&second.id).await.unwrap().state,
            RolloutState::Complete
        );
    }

    #[tokio::test]
    async fn a_human_with_a_grant_releases_a_held_split_and_a_second_rollout_starts() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.run(&first.id).await.unwrap();
        let log = RolloutLog::open(&rig.path).unwrap();
        let grant = ResolveGrant::check(true, None).unwrap();

        let running = rig.executor.start(plan("sandbox"), V3).await.unwrap();
        assert!(matches!(
            release_halted_lease(&grant, &log, &*rig.leases, &running.id).unwrap_err(),
            RolloutError::NotHalted(_)
        ));
        assert_eq!(held_by(&rig), std::slice::from_ref(&first.id));

        let released = release_halted_lease(&grant, &log, &*rig.leases, &first.id).unwrap();
        assert_eq!(released.state, RolloutState::Halted);
        assert!(held_by(&rig).is_empty());
        rig.audit.deny_from(99, "reset");
        assert_eq!(
            rig.executor.run(&running.id).await.unwrap().state,
            RolloutState::Complete
        );
        assert!(matches!(
            release_halted_lease(&grant, &log, &*rig.leases, "nope").unwrap_err(),
            RolloutError::UnknownRollout(_)
        ));
    }

    #[tokio::test]
    async fn the_release_command_needs_a_grant_a_lease_log_and_a_halted_rollout() {
        let rig = rig();
        rig.audit.deny_from(1, "no");
        let first = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.executor.run(&first.id).await.unwrap();
        let log = RolloutLog::open(&rig.path).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let lease_path = dir.path().join("leases.jsonl");
        let store = crate::leases::JsonlLeaseStore::open(&lease_path).unwrap();
        store
            .acquire(
                &LeaseName::deploy("registry.example.invalid/ns/app", "sandbox"),
                &first.id,
                Duration::days(1),
                t0(),
            )
            .unwrap();
        let grant = || ResolveGrant::check(true, None);
        let agent = || ResolveGrant::check(true, Some("task-1"));
        assert!(
            release_halted_lease_cli(&log, agent(), Some(lease_path.clone()), &first.id)
                .unwrap_err()
                .to_string()
                .contains("agent session")
        );
        assert!(release_halted_lease_cli(&log, grant(), None, &first.id)
            .unwrap_err()
            .to_string()
            .contains("no lease log"));
        let line =
            release_halted_lease_cli(&log, grant(), Some(lease_path.clone()), &first.id).unwrap();
        assert!(line.starts_with("released rollout-"), "{line}");
        let reopened = crate::leases::JsonlLeaseStore::open(&lease_path).unwrap();
        assert!(reopened.snapshot().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_operator_halt_keeps_the_lease_only_while_traffic_is_routed() {
        let rig = rig();
        let mut live = RolloutRecord::new("rollout-live", plan("sandbox"), V2, V1, t0());
        live.state = RolloutState::Step(1);
        live.traffic_percent = 10;
        rig.executor.log().append(None, &mut live).unwrap();
        rig.leases
            .acquire(
                &live.lease_name().unwrap(),
                "rollout-live",
                Duration::minutes(5),
                t0(),
            )
            .unwrap();
        rig.executor.halt("rollout-live").unwrap();
        rig.clock.advance(Duration::days(30));
        assert_eq!(held_by(&rig), ["rollout-live".to_string()]);
        assert!(rig.leases.expired(rig.clock.now()).unwrap().is_empty());

        let rig = self::rig();
        let idle = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        rig.leases
            .acquire(
                &idle.lease_name().unwrap(),
                &idle.id,
                Duration::minutes(5),
                t0(),
            )
            .unwrap();
        rig.executor.halt(&idle.id).unwrap();
        assert!(held_by(&rig).is_empty());
    }

    #[tokio::test]
    async fn a_failed_rollback_on_the_rollback_policy_escalates() {
        let rig = rig();
        rig.health.push_after(5, breach_sample());
        rig.adapter.fail(AdapterOp::RollbackTo);
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let err = rig.executor.run(&record.id).await.unwrap_err();
        assert!(matches!(err, RolloutError::Adapter(_)));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("rollback failed"));
        assert_eq!(
            rig.executor.status(&record.id).unwrap().state,
            RolloutState::RollingBack
        );
    }

    #[tokio::test]
    async fn a_failing_shadow_source_halts_and_escalates() {
        let rig = rig();
        rig.shadow.set_failing(true);
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let done = rig.executor.run(&record.id).await.unwrap();
        assert_eq!(done.state, RolloutState::Halted);
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("shadow"));
    }

    #[tokio::test]
    async fn a_lease_store_failure_escalates_and_still_surfaces() {
        let rig = rig();
        let executor = rig.executor.with_config(RolloutConfig {
            poll_interval: Duration::minutes(1),
            lease_grace: Duration::hours(-9),
        });
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(matches!(
            executor.run(&record.id).await.unwrap_err(),
            RolloutError::Lease(LeaseError::NonPositiveTtl(_))
        ));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("lease"));
    }

    #[tokio::test]
    async fn a_failing_escalation_does_not_mask_the_original_stop_error() {
        let rig = rig();
        rig.escalation.set_failing(true);
        let executor = rig.executor.with_config(RolloutConfig {
            poll_interval: Duration::minutes(1),
            lease_grace: Duration::hours(-9),
        });
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        assert!(matches!(
            executor.run(&record.id).await.unwrap_err(),
            RolloutError::Lease(LeaseError::NonPositiveTtl(_))
        ));
        assert_eq!(rig.escalation.escalations().len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_window_escalates_and_still_surfaces() {
        let rig = rig();
        let executor = RolloutExecutor::new(
            RolloutLog::open(&rig.path).unwrap(),
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            rig.adapter.clone(),
            rig.health.clone(),
        )
        .with_escalation(rig.escalation.clone());
        let record = executor.start(plan("production"), V2).await.unwrap();
        assert!(matches!(
            executor.run(&record.id).await.unwrap_err(),
            RolloutError::Window(_)
        ));
        let escalations = rig.escalation.escalations();
        assert_eq!(escalations.len(), 1);
        assert!(escalations[0].summary.contains("window"));
    }

    fn held(repo: &str) -> Arc<EscalationLog> {
        use crate::escalation::{Escalation, EscalationSource, Severity};
        let log = Arc::new(EscalationLog::in_memory());
        let incident = Escalation::new(
            Severity::Incident,
            EscalationSource::Rollout,
            repo,
            "p99 breached",
        )
        .with_id("inc-1");
        log.hold(&incident, t0()).unwrap();
        log
    }

    #[tokio::test]
    async fn an_incident_hold_parks_a_production_rollout_before_any_effect() {
        let rig = rig();
        let log = held("example/repo");
        let executor = rig
            .executor
            .with_production_hold(log.clone(), Some("example/repo".into()));
        let record = executor.start(plan("production"), V2).await.unwrap();
        let parked = executor.run(&record.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
        assert!(rig
            .adapter
            .calls()
            .iter()
            .all(|c| *c == AdapterCall::CurrentImage));
        assert!(rig.leases.snapshot().unwrap().is_empty());
        assert!(rig.escalation.escalations().is_empty());
        let grant = crate::escalation::ResolveGrant::check(true, None).unwrap();
        log.resolve(&grant, "inc-1", t0()).unwrap();
        let RolloutState::Parked { until, .. } = parked.state else {
            unreachable!()
        };
        rig.clock.advance(until - rig.clock.now());
        let resumed = executor.run(&record.id).await.unwrap();
        assert!(rig
            .adapter
            .calls()
            .iter()
            .any(|c| matches!(c, AdapterCall::DeployInactive(_))));
        assert_ne!(resumed.state, RolloutState::Step(0));
    }

    #[tokio::test]
    async fn a_production_halt_reaches_the_sink_and_parks_the_next_production_rollout() {
        use crate::escalation::{
            DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink,
            Escalator,
        };
        struct Capture(std::sync::Mutex<Vec<Escalation>>);
        #[async_trait]
        impl EscalationSink for Capture {
            fn name(&self) -> &str {
                "capture"
            }
            async fn deliver(&self, e: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
                self.0.lock().unwrap().push(e.clone());
                Ok(DeliveryReceipt {
                    sink: "capture".into(),
                    reference: e.id.clone(),
                    outcome: DeliveryOutcome::Posted,
                })
            }
        }
        let rig = rig();
        let sink = Arc::new(Capture(Default::default()));
        let log = Arc::new(EscalationLog::in_memory());
        let escalator = Arc::new(Escalator::new(
            log.clone(),
            sink.clone(),
            rig.clock.clone(),
            Duration::minutes(30),
        ));
        let executor = rig.executor.with_escalator(escalator, "example/repo");
        rig.health.push_after(3, breach_sample());
        let plan = plan_with("[rollback]\nautomatic = true\non_breach = \"halt-and-escalate\"\n");
        let mut production = plan.clone();
        production.environment = "production".into();
        production.lease =
            DeployPlan::lease_name("registry.example.invalid/ns", "app", "production");
        for step in &mut production.steps {
            step.preconditions.clear();
        }
        let first = executor.start(production.clone(), V2).await.unwrap();
        let halted = executor.run(&first.id).await.unwrap();
        assert_eq!(halted.state, RolloutState::Halted);
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        assert!(log.production_held("example/repo"));
        let second = executor.start(production, V3).await.unwrap();
        let parked = executor.run(&second.id).await.unwrap();
        assert!(matches!(parked.state, RolloutState::Parked { .. }));
    }

    #[tokio::test]
    async fn an_incident_hold_leaves_other_repos_and_other_environments_alone() {
        let rig = rig();
        let other = rig
            .executor
            .with_production_hold(held("example/repo"), Some("example/other".into()));
        let record = other.start(plan("sandbox"), V2).await.unwrap();
        assert_eq!(
            other.run(&record.id).await.unwrap().state,
            RolloutState::Complete
        );
    }

    #[tokio::test]
    async fn an_unscoped_hold_does_not_stop_a_sandbox_rollout() {
        let rig = rig();
        let executor = rig
            .executor
            .with_production_hold(held("example/repo"), None);
        let record = executor.start(plan("sandbox"), V2).await.unwrap();
        assert_eq!(
            executor.run(&record.id).await.unwrap().state,
            RolloutState::Complete
        );
    }

    #[tokio::test]
    async fn list_orders_by_creation_and_fake_executor_dry_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let (executor, adapter, health, _shadow, _clock) =
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
        assert_eq!(health.calls().len(), 3 * 31);
    }

    const BLUE_GREEN_HEALTH: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n";

    fn blue_green_plan() -> DeployPlan {
        DeployTemplate::parse(BLUE_GREEN_HEALTH)
            .unwrap()
            .plan("sandbox")
            .unwrap()
    }

    fn halt_now(log: &RolloutLog) {
        let id = log.latest().unwrap().into_keys().next().unwrap();
        log.halt(&id, t0()).unwrap();
    }

    struct HaltAfterCall {
        inner: Arc<FakeAdapter>,
        log: RolloutLog,
        trigger: AdapterCall,
        fired: std::sync::atomic::AtomicBool,
    }

    impl HaltAfterCall {
        fn after(&self) {
            let last = self.inner.calls().last().cloned();
            if last.as_ref() == Some(&self.trigger)
                && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                halt_now(&self.log);
            }
        }
    }

    #[async_trait]
    impl TargetAdapter for HaltAfterCall {
        async fn deploy_inactive(
            &self,
            image: &str,
        ) -> Result<Slot, crate::rollout::adapter::AdapterError> {
            let out = self.inner.deploy_inactive(image).await;
            self.after();
            out
        }
        async fn set_traffic(
            &self,
            slot: &Slot,
            percent: u8,
        ) -> Result<(), crate::rollout::adapter::AdapterError> {
            let out = self.inner.set_traffic(slot, percent).await;
            self.after();
            out
        }
        async fn current_image(&self) -> Result<String, crate::rollout::adapter::AdapterError> {
            let out = self.inner.current_image().await;
            self.after();
            out
        }
        async fn rollback_to(
            &self,
            image: &str,
        ) -> Result<(), crate::rollout::adapter::AdapterError> {
            let out = self.inner.rollback_to(image).await;
            self.after();
            out
        }
        async fn retire(&self, slot: &Slot) -> Result<(), crate::rollout::adapter::AdapterError> {
            let out = self.inner.retire(slot).await;
            self.after();
            out
        }
        async fn mirror(
            &self,
            slot: &Slot,
            percent: u8,
        ) -> Result<(), crate::rollout::adapter::AdapterError> {
            let out = self.inner.mirror(slot, percent).await;
            self.after();
            out
        }
        async fn swap(
            &self,
        ) -> Result<crate::rollout::adapter::Swapped, crate::rollout::adapter::AdapterError>
        {
            let out = self.inner.swap().await;
            self.after();
            out
        }
        async fn set_fallback(
            &self,
            slot: &Slot,
            policy: &FallbackPolicy,
        ) -> Result<FallbackSupport, crate::rollout::adapter::AdapterError> {
            let out = self.inner.set_fallback(slot, policy).await;
            self.after();
            out
        }
        async fn clear_fallback(
            &self,
            slot: &Slot,
        ) -> Result<(), crate::rollout::adapter::AdapterError> {
            let out = self.inner.clear_fallback(slot).await;
            self.after();
            out
        }
    }

    fn executor_halting_after(rig: &Rig, trigger: AdapterCall) -> RolloutExecutor {
        let log = RolloutLog::open(&rig.path).unwrap();
        let adapter = Arc::new(HaltAfterCall {
            inner: rig.adapter.clone(),
            log: log.clone(),
            trigger,
            fired: std::sync::atomic::AtomicBool::new(false),
        });
        RolloutExecutor::new(
            log,
            rig.leases.clone(),
            WindowSet::default(),
            rig.clock.clone(),
            adapter,
            rig.health.clone(),
        )
        .with_shadow_source(rig.shadow.clone())
        .with_config(one_poll_per_step())
    }

    async fn stale_record(rig: &Rig, plan: DeployPlan) -> RolloutRecord {
        let record = rig.executor.start(plan, V2).await.unwrap();
        rig.executor.halt(&record.id).unwrap();
        record
    }

    fn assert_conflict<T: std::fmt::Debug>(outcome: Result<T, RolloutError>) {
        assert!(
            matches!(outcome, Err(RolloutError::Conflict { .. })),
            "{outcome:?}"
        );
    }

    fn effects(rig: &Rig) -> Vec<AdapterCall> {
        rig.adapter
            .calls()
            .into_iter()
            .filter(|c| *c != AdapterCall::CurrentImage)
            .collect()
    }

    #[tokio::test]
    async fn a_halt_before_the_deploy_stops_deploy_inactive() {
        let rig = rig();
        let record = stale_record(&rig, plan("sandbox")).await;
        assert_conflict(rig.executor.step(record, 0).await);
        assert_eq!(effects(&rig), vec![]);
    }

    #[tokio::test]
    async fn a_halt_before_the_fallback_stops_set_fallback() {
        let rig = rig();
        let mut record = stale_record(&rig, plan("sandbox")).await;
        record.slot = Some(Slot::new("slot-1"));
        assert_conflict(rig.executor.step(record, 0).await);
        assert_eq!(effects(&rig), vec![]);
    }

    #[tokio::test]
    async fn a_halt_before_the_mirror_stops_the_shadow_step() {
        let rig = rig();
        let mut record = stale_record(&rig, shadow_plan()).await;
        record.slot = Some(Slot::new("slot-1"));
        record.fallback = Some(FallbackSupport::Native);
        assert_conflict(rig.executor.step(record, 0).await);
        assert_eq!(effects(&rig), vec![]);
    }

    #[tokio::test]
    async fn a_halt_before_the_swap_stops_the_swap() {
        let rig = rig();
        let mut record = stale_record(&rig, blue_green_plan()).await;
        record.state = RolloutState::Step(0);
        record.slot = Some(Slot::new("slot-1"));
        record.fallback = Some(FallbackSupport::Native);
        assert_conflict(rig.executor.step(record, 0).await);
        assert_eq!(effects(&rig), vec![]);
    }

    fn retire_ready(mut record: RolloutRecord) -> RolloutRecord {
        record.slot = Some(Slot::new("slot-1"));
        record.fallback = Some(FallbackSupport::Native);
        record.retained_slot = Some(Slot::new("slot-0"));
        record.retained_since = Some(t0() - Duration::days(3));
        record
    }

    #[tokio::test]
    async fn a_halt_before_the_retired_slots_fallback_is_cleared_stops_clear_fallback() {
        let rig = rig();
        let record = retire_ready(stale_record(&rig, blue_green_plan()).await);
        assert_conflict(rig.executor.step(record, 1).await);
        assert_eq!(effects(&rig), vec![]);
    }

    #[tokio::test]
    async fn a_halt_after_the_retired_slots_fallback_is_cleared_stops_retire() {
        let rig = rig();
        let record = rig.executor.start(blue_green_plan(), V2).await.unwrap();
        let record = retire_ready(record);
        let executor = executor_halting_after(
            &rig,
            AdapterCall::ClearFallback(record.retained_slot.clone().unwrap()),
        );
        assert_conflict(executor.step(record, 1).await);
        let calls = effects(&rig);
        assert_eq!(calls, vec![AdapterCall::ClearFallback(Slot::new("slot-0"))]);
    }

    #[tokio::test]
    async fn a_halt_before_the_final_clear_fallback_stops_completion() {
        let rig = rig();
        let mut record = stale_record(&rig, plan("sandbox")).await;
        record.slot = Some(Slot::new("slot-1"));
        let last = record.plan.steps.len() - 1;
        assert_conflict(rig.executor.advance_or_complete(record, last).await);
        assert_eq!(effects(&rig), vec![]);
    }

    struct HaltOnNthNow {
        inner: Arc<SimulatedClock>,
        log: RolloutLog,
        nth: usize,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Clock for HaltOnNthNow {
        fn now(&self) -> DateTime<Utc> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n == self.nth {
                halt_now(&self.log);
            }
            self.inner.now()
        }

        fn sleep(&self, duration: Duration) -> crate::leases::SleepFuture<'_> {
            self.inner.sleep(duration)
        }
    }

    #[tokio::test]
    async fn a_halt_at_the_end_of_a_shadow_bake_stops_the_mirror_reset() {
        let rig = rig();
        let record = rig.executor.start(shadow_plan(), V2).await.unwrap();
        let log = RolloutLog::open(&rig.path).unwrap();
        let clock = Arc::new(HaltOnNthNow {
            inner: rig.clock.clone(),
            log: log.clone(),
            nth: 3,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let executor = RolloutExecutor::new(
            log,
            rig.leases.clone(),
            WindowSet::default(),
            clock,
            rig.adapter.clone(),
            rig.health.clone(),
        )
        .with_shadow_source(rig.shadow.clone())
        .with_config(one_poll_per_step());
        let mut record = record;
        record.slot = Some(Slot::new("slot-1"));
        record.plan.steps[0].min_duration = Duration::zero();
        assert_conflict(executor.bake(record, 0, t0()).await);
        assert_eq!(effects(&rig), vec![]);
    }

    fn rolling_back(record: RolloutRecord) -> RolloutRecord {
        let mut record = retire_ready(record);
        record.state = RolloutState::RollingBack;
        record
    }

    #[tokio::test]
    async fn a_halt_before_a_rollback_stops_restoring_the_retained_slot() {
        let rig = rig();
        let record = rolling_back(stale_record(&rig, blue_green_plan()).await);
        assert_conflict(rig.executor.roll_back(record).await);
        assert_eq!(effects(&rig), vec![]);
    }

    async fn rollback_halted_after(trigger: AdapterCall) -> Vec<AdapterCall> {
        let rig = rig();
        let slot = rig.adapter.deploy_inactive(V2).await.unwrap();
        rig.adapter
            .set_fallback(&slot, &FallbackPolicy::default())
            .await
            .unwrap();
        let record = rig.executor.start(blue_green_plan(), V2).await.unwrap();
        let record = rolling_back(record);
        let executor = executor_halting_after(&rig, trigger);
        assert_conflict(executor.roll_back(record).await);
        effects(&rig)[2..].to_vec()
    }

    #[tokio::test]
    async fn a_halt_during_a_resumed_rollback_ends_the_run_halted_without_a_human_stop() {
        let rig = rig();
        let record = rig.executor.start(plan("sandbox"), V2).await.unwrap();
        let slot = rig.adapter.deploy_inactive(V2).await.unwrap();
        let mut crafted = record.clone();
        crafted.state = RolloutState::RollingBack;
        crafted.slot = Some(slot.clone());
        rig.executor
            .log()
            .append(Some(&record.state), &mut crafted)
            .unwrap();
        let executor = executor_halting_after(&rig, AdapterCall::SetTraffic(slot, 0));
        let finished = executor.run(&record.id).await.unwrap();
        assert_eq!(finished.state, RolloutState::Halted);
        assert!(!effects(&rig)
            .iter()
            .any(|c| matches!(c, AdapterCall::RollbackTo(_))));
    }

    #[tokio::test]
    async fn a_halt_after_restoring_the_retained_slot_stops_draining_the_candidate() {
        let old = Slot::new("slot-0");
        assert_eq!(
            rollback_halted_after(AdapterCall::SetTraffic(old.clone(), 100)).await,
            vec![AdapterCall::SetTraffic(old, 100)]
        );
    }

    #[tokio::test]
    async fn a_halt_after_draining_the_candidate_stops_the_mirror_reset() {
        let slot = Slot::new("slot-1");
        assert_eq!(
            rollback_halted_after(AdapterCall::SetTraffic(slot.clone(), 0)).await,
            vec![
                AdapterCall::SetTraffic(Slot::new("slot-0"), 100),
                AdapterCall::SetTraffic(slot, 0)
            ]
        );
    }

    #[tokio::test]
    async fn a_halt_after_the_mirror_reset_stops_clear_fallback_in_a_rollback() {
        let slot = Slot::new("slot-1");
        assert_eq!(
            rollback_halted_after(AdapterCall::Mirror(slot.clone(), 0)).await,
            vec![
                AdapterCall::SetTraffic(Slot::new("slot-0"), 100),
                AdapterCall::SetTraffic(slot.clone(), 0),
                AdapterCall::Mirror(slot, 0)
            ]
        );
    }

    #[tokio::test]
    async fn a_halt_after_clearing_the_candidates_fallback_stops_rollback_to() {
        let slot = Slot::new("slot-1");
        assert_eq!(
            rollback_halted_after(AdapterCall::ClearFallback(slot.clone())).await,
            vec![
                AdapterCall::SetTraffic(Slot::new("slot-0"), 100),
                AdapterCall::SetTraffic(slot.clone(), 0),
                AdapterCall::Mirror(slot.clone(), 0),
                AdapterCall::ClearFallback(slot)
            ]
        );
    }
}
