use super::adapter::{FallbackSupport, Slot};
use super::health::HealthBreach;
use super::RolloutError;
use crate::deploy::{DeployPlan, DeployStep};
use crate::leases::LeaseName;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Where a rollout is in its plan.
///
/// `Pending → Step(0) → Baking(0) → Step(1) … → Complete`, with detours to
/// `RollingBack → RolledBack` on a breach, `Halted` on a breach that holds
/// the split or on the human kill switch, and `Parked` while a precondition
/// (window, lease) is not met. Every transition is checked by
/// [`RolloutRecord::transition`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutState {
    /// Created, nothing applied yet.
    Pending,
    /// About to apply step `n`: preconditions are checked and traffic set.
    Step(usize),
    /// Step `n` is applied; health is polled until the bake and hold end.
    Baking {
        /// Step being observed.
        step: usize,
        /// When the step's traffic was applied.
        since: DateTime<Utc>,
    },
    /// Every step finished; the new image serves 100%.
    Complete,
    /// The previous image is being restored.
    RollingBack,
    /// The previous image serves 100% again.
    RolledBack,
    /// The current traffic split is held until a human acts.
    Halted,
    /// Waiting for a precondition; resumes at `resume_state` from `until`.
    Parked {
        /// Earliest instant to try again.
        until: DateTime<Utc>,
        /// State to re-enter when resuming, always a `Step`.
        resume_state: Box<RolloutState>,
    },
}

impl RolloutState {
    /// Short name used in logs and the CLI.
    pub fn name(&self) -> &'static str {
        match self {
            RolloutState::Pending => "pending",
            RolloutState::Step(_) => "step",
            RolloutState::Baking { .. } => "baking",
            RolloutState::Complete => "complete",
            RolloutState::RollingBack => "rolling-back",
            RolloutState::RolledBack => "rolled-back",
            RolloutState::Halted => "halted",
            RolloutState::Parked { .. } => "parked",
        }
    }

    /// `Complete`, `RolledBack` and `Halted` accept no further transition
    /// except a roll-forward out of `Halted`.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RolloutState::Complete | RolloutState::RolledBack | RolloutState::Halted
        )
    }

    /// The step this state is at, if any (`Parked` reports its resume step).
    pub fn step(&self) -> Option<usize> {
        match self {
            RolloutState::Step(n) | RolloutState::Baking { step: n, .. } => Some(*n),
            RolloutState::Parked { resume_state, .. } => resume_state.step(),
            _ => None,
        }
    }

    /// Whether `next` may follow `self` in a plan of `steps` steps.
    pub fn allows(&self, next: &RolloutState, steps: usize) -> bool {
        use RolloutState::*;
        if matches!(next, Halted) {
            return !self.is_terminal();
        }
        match (self, next) {
            (Pending, Step(0)) => true,
            (Step(n), Baking { step, .. }) => step == n,
            (Step(n), Parked { resume_state, .. }) => **resume_state == Step(*n),
            (Step(_) | Baking { .. } | Halted, Step(0)) => true,
            (Baking { step, .. }, Step(m)) => *m == step + 1 && *m < steps,
            (Baking { step, .. }, Complete) => step + 1 == steps,
            (Step(_) | Baking { .. }, RollingBack) => true,
            (RollingBack, RolledBack) => true,
            (Parked { resume_state, .. }, resumed) => **resume_state == *resumed,
            _ => false,
        }
    }
}

impl fmt::Display for RolloutState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RolloutState::Step(n) => write!(f, "step {n}"),
            RolloutState::Baking { step, since } => write!(f, "baking step {step} since {since}"),
            RolloutState::Parked {
                until,
                resume_state,
            } => write!(f, "parked until {until} (resume at {resume_state})"),
            other => f.write_str(other.name()),
        }
    }
}

/// Everything the executor needs to resume a rollout after a crash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloutRecord {
    /// Unique id; also the lease holder.
    pub id: String,
    /// The plan being executed.
    pub plan: DeployPlan,
    /// Image reference being rolled out.
    pub image: String,
    /// Image that served 100% before the rollout started.
    pub previous_image: String,
    /// Slot the new image is deployed to, once it is.
    pub slot: Option<Slot>,
    /// The slot a `Swap` step retired the previous image to; held until a
    /// `Retire` step's `min_duration` (`rollback.retain_for`) elapses, so
    /// `rollback_to` can restore it.
    pub retained_slot: Option<Slot>,
    /// How faithfully the target honours the fallback policy installed on
    /// the new slot for the duration of the rollout; `None` before it is
    /// installed. `BestEffort` is worth surfacing to an operator: a `5xx`
    /// may reach clients rather than being retried against the old slot.
    pub fallback: Option<FallbackSupport>,
    /// Share of live traffic the new image serves right now.
    pub traffic_percent: u8,
    /// Current state.
    pub state: RolloutState,
    /// Pull request that justified a roll-forward, when one happened.
    pub pr: Option<String>,
    /// Breach that ended the last step, if any.
    pub breach: Option<HealthBreach>,
    /// When the record was created.
    pub created_at: DateTime<Utc>,
    /// When the record last changed.
    pub updated_at: DateTime<Utc>,
}

impl RolloutRecord {
    /// A `Pending` record for rolling `image` out under `plan`.
    pub fn new(
        id: impl Into<String>,
        plan: DeployPlan,
        image: &str,
        previous_image: &str,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            plan,
            image: image.to_string(),
            previous_image: previous_image.to_string(),
            slot: None,
            retained_slot: None,
            fallback: None,
            traffic_percent: 0,
            state: RolloutState::Pending,
            pr: None,
            breach: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Move to `next`, refusing anything [`RolloutState::allows`] does not.
    ///
    /// ```
    /// use chrono::Utc;
    /// use harness::deploy::DeployTemplate;
    /// use harness::rollout::{RolloutError, RolloutRecord, RolloutState};
    ///
    /// let plan = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
    /// )
    /// .unwrap()
    /// .plan("sandbox")
    /// .unwrap();
    /// let now = Utc::now();
    /// let mut record = RolloutRecord::new("rollout-1", plan, "registry.example.invalid/ns/app:v2", "registry.example.invalid/ns/app:v1", now);
    /// record.transition(RolloutState::Step(0), now).unwrap();
    /// record.transition(RolloutState::Baking { step: 0, since: now }, now).unwrap();
    /// let err = record.transition(RolloutState::Step(1), now).unwrap_err();
    /// assert!(matches!(err, RolloutError::InvalidTransition { .. }));
    /// record.transition(RolloutState::Complete, now).unwrap();
    /// assert!(record.state.is_terminal());
    /// ```
    pub fn transition(
        &mut self,
        next: RolloutState,
        now: DateTime<Utc>,
    ) -> Result<(), RolloutError> {
        if !self.state.allows(&next, self.plan.steps.len()) {
            return Err(RolloutError::InvalidTransition {
                id: self.id.clone(),
                from: self.state.clone(),
                to: next,
            });
        }
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    /// The step the record is at, if any.
    pub fn current_step(&self) -> Option<&DeployStep> {
        self.state.step().and_then(|n| self.plan.steps.get(n))
    }

    /// Highest traffic percent the current state may apply to the new image.
    ///
    /// A step may never exceed its own plan percent; a halted rollout holds
    /// what it has; a rollback drives the new image to zero.
    pub fn traffic_ceiling(&self) -> u8 {
        match &self.state {
            RolloutState::Pending | RolloutState::RollingBack | RolloutState::RolledBack => 0,
            RolloutState::Complete => 100,
            RolloutState::Halted => self.traffic_percent,
            RolloutState::Step(_) | RolloutState::Baking { .. } | RolloutState::Parked { .. } => {
                self.current_step().map_or(0, |s| s.traffic_percent)
            }
        }
    }

    /// Record `percent` as applied, refusing anything above
    /// [`traffic_ceiling`](Self::traffic_ceiling). The executor calls this
    /// before touching the adapter, so a refused percent is never applied.
    pub fn set_traffic(&mut self, percent: u8) -> Result<(), RolloutError> {
        let ceiling = self.traffic_ceiling();
        if percent > ceiling {
            return Err(RolloutError::TrafficExceedsStep {
                id: self.id.clone(),
                requested: percent,
                ceiling,
            });
        }
        self.traffic_percent = percent;
        Ok(())
    }

    /// The deploy lease every step requires, parsed from the plan's
    /// `deploy:<repo>:<env>` lease string.
    pub fn lease_name(&self) -> Result<LeaseName, RolloutError> {
        let env = &self.plan.environment;
        let repo = self
            .plan
            .lease
            .strip_prefix("deploy:")
            .and_then(|rest| rest.strip_suffix(&format!(":{env}")))
            .filter(|repo| !repo.is_empty());
        match repo {
            Some(repo) => Ok(LeaseName::deploy(repo, env)),
            None => Err(RolloutError::BadLeaseName(self.plan.lease.clone())),
        }
    }

    /// One-line summary for the CLI.
    pub fn summary(&self) -> String {
        let retained = self
            .retained_slot
            .as_ref()
            .map_or_else(String::new, |slot| format!("  retained: {slot}"));
        let fallback = match self.fallback {
            Some(FallbackSupport::BestEffort) => "  fallback: best-effort",
            _ => "",
        };
        format!(
            "{}  {}  {} -> {}  traffic {}%  state: {}{retained}{fallback}",
            self.id,
            self.plan.environment,
            self.previous_image,
            self.image,
            self.traffic_percent,
            self.state
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::deploy::DeployTemplate;
    use chrono::TimeZone;
    use proptest::prelude::{any, prop, prop_assert_eq, proptest, Strategy as _};

    pub(crate) const GRADUAL: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\", \"production\"]\n[risk]\nclass = \"edge\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [10, 50, 100]\nmin_step_duration = \"8h\"\nwindows = \"business-hours\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\n";

    pub(crate) fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 28, 8, 0, 0).unwrap()
    }

    pub(crate) fn plan(env: &str) -> DeployPlan {
        DeployTemplate::parse(GRADUAL).unwrap().plan(env).unwrap()
    }

    pub(crate) fn record(env: &str) -> RolloutRecord {
        RolloutRecord::new(
            "rollout-1",
            plan(env),
            "registry.example.invalid/ns/app:v2",
            "registry.example.invalid/ns/app:v1",
            t0(),
        )
    }

    fn baking(step: usize) -> RolloutState {
        RolloutState::Baking { step, since: t0() }
    }

    fn parked(step: usize) -> RolloutState {
        RolloutState::Parked {
            until: t0(),
            resume_state: Box::new(RolloutState::Step(step)),
        }
    }

    fn every_state() -> Vec<RolloutState> {
        vec![
            RolloutState::Pending,
            RolloutState::Step(0),
            RolloutState::Step(1),
            RolloutState::Step(2),
            baking(0),
            baking(1),
            baking(2),
            RolloutState::Complete,
            RolloutState::RollingBack,
            RolloutState::RolledBack,
            RolloutState::Halted,
            parked(0),
            parked(1),
        ]
    }

    #[test]
    fn happy_path_walks_every_step_to_complete() {
        let mut r = record("production");
        r.transition(RolloutState::Step(0), t0()).unwrap();
        r.transition(baking(0), t0()).unwrap();
        r.transition(RolloutState::Step(1), t0()).unwrap();
        r.transition(baking(1), t0()).unwrap();
        r.transition(RolloutState::Step(2), t0()).unwrap();
        r.transition(baking(2), t0()).unwrap();
        r.transition(RolloutState::Complete, t0()).unwrap();
        assert!(r.state.is_terminal());
        assert_eq!(r.updated_at, t0());
    }

    #[test]
    fn transition_table_is_exhaustive() {
        use RolloutState::*;
        let steps = 3;
        for from in every_state() {
            for to in every_state() {
                let expected = match (&from, &to) {
                    (_, Halted) => !from.is_terminal(),
                    (Pending, Step(0)) => true,
                    (Step(n), Baking { step, .. }) => step == n,
                    (Step(n), Parked { resume_state, .. }) => **resume_state == Step(*n),
                    (Step(_) | Baking { .. } | Halted, Step(0)) => true,
                    (Baking { step, .. }, Step(m)) => *m == step + 1,
                    (Baking { step, .. }, Complete) => *step == 2,
                    (Step(_) | Baking { .. }, RollingBack) => true,
                    (RollingBack, RolledBack) => true,
                    (Parked { resume_state, .. }, to) => **resume_state == *to,
                    _ => false,
                };
                assert_eq!(from.allows(&to, steps), expected, "{from} -> {to}");
            }
        }
    }

    #[test]
    fn baking_last_step_completes_and_never_overruns() {
        assert!(baking(2).allows(&RolloutState::Complete, 3));
        assert!(!baking(2).allows(&RolloutState::Step(3), 3));
        assert!(!baking(1).allows(&RolloutState::Complete, 3));
        assert!(baking(0).allows(&RolloutState::Complete, 1));
    }

    #[test]
    fn terminal_states_refuse_everything_but_roll_forward_from_halted() {
        for from in [RolloutState::Complete, RolloutState::RolledBack] {
            for to in every_state() {
                assert!(!from.allows(&to, 3), "{from} -> {to}");
            }
        }
        for to in every_state() {
            assert_eq!(
                RolloutState::Halted.allows(&to, 3),
                to == RolloutState::Step(0),
                "halted -> {to}"
            );
        }
    }

    #[test]
    fn invalid_transition_names_both_states_and_keeps_the_record() {
        let mut r = record("production");
        let err = r.transition(RolloutState::Step(1), t0()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "rollout rollout-1 cannot move from pending to step 1"
        );
        assert_eq!(r.state, RolloutState::Pending);
    }

    #[test]
    fn step_and_current_step_follow_parked_and_baking() {
        let mut r = record("production");
        assert_eq!(r.current_step(), None);
        assert_eq!(RolloutState::Complete.step(), None);
        r.transition(RolloutState::Step(1), t0()).unwrap_err();
        r.transition(RolloutState::Step(0), t0()).unwrap();
        r.transition(parked(0), t0()).unwrap();
        assert_eq!(r.current_step().unwrap().index, 0);
        r.transition(RolloutState::Step(0), t0()).unwrap();
        r.transition(baking(0), t0()).unwrap();
        r.transition(RolloutState::Step(1), t0()).unwrap();
        assert_eq!(r.current_step().unwrap().traffic_percent, 50);
    }

    #[test]
    fn traffic_ceiling_per_state() {
        let mut r = record("production");
        assert_eq!(r.traffic_ceiling(), 0);
        r.state = RolloutState::Step(1);
        assert_eq!(r.traffic_ceiling(), 50);
        r.state = baking(2);
        assert_eq!(r.traffic_ceiling(), 100);
        r.state = parked(1);
        assert_eq!(r.traffic_ceiling(), 50);
        r.state = RolloutState::Step(7);
        assert_eq!(r.traffic_ceiling(), 0);
        r.state = RolloutState::Complete;
        assert_eq!(r.traffic_ceiling(), 100);
        r.state = RolloutState::RollingBack;
        assert_eq!(r.traffic_ceiling(), 0);
        r.state = RolloutState::RolledBack;
        assert_eq!(r.traffic_ceiling(), 0);
        r.traffic_percent = 37;
        r.state = RolloutState::Halted;
        assert_eq!(r.traffic_ceiling(), 37);
    }

    #[test]
    fn set_traffic_refuses_more_than_the_step_allows() {
        let mut r = record("production");
        r.state = RolloutState::Step(1);
        r.set_traffic(50).unwrap();
        assert_eq!(r.traffic_percent, 50);
        let err = r.set_traffic(51).unwrap_err();
        assert_eq!(
            err.to_string(),
            "rollout rollout-1 asked for 51% traffic but step allows at most 50%"
        );
        assert_eq!(r.traffic_percent, 50);
        r.set_traffic(0).unwrap();
    }

    #[test]
    fn lease_name_is_parsed_from_the_plan() {
        let r = record("production");
        assert_eq!(
            r.lease_name().unwrap(),
            LeaseName::deploy("app", "production")
        );
        let mut odd = record("sandbox");
        odd.plan.lease = "branch:app:main".into();
        assert!(
            matches!(odd.lease_name().unwrap_err(), RolloutError::BadLeaseName(name) if name == "branch:app:main")
        );
        odd.plan.lease = "deploy::sandbox".into();
        assert!(odd.lease_name().is_err());
    }

    #[test]
    fn display_names_and_summary() {
        assert_eq!(RolloutState::Pending.to_string(), "pending");
        assert_eq!(RolloutState::Step(2).to_string(), "step 2");
        assert_eq!(
            baking(1).to_string(),
            format!("baking step 1 since {}", t0())
        );
        assert_eq!(
            parked(1).to_string(),
            format!("parked until {} (resume at step 1)", t0())
        );
        assert_eq!(RolloutState::RollingBack.name(), "rolling-back");
        assert_eq!(RolloutState::RolledBack.name(), "rolled-back");
        assert_eq!(RolloutState::Complete.name(), "complete");
        assert_eq!(RolloutState::Halted.name(), "halted");
        let mut r = record("production");
        assert_eq!(
            r.summary(),
            "rollout-1  production  registry.example.invalid/ns/app:v1 -> registry.example.invalid/ns/app:v2  traffic 0%  state: pending"
        );
        r.retained_slot = Some(Slot::new("slot-0"));
        assert_eq!(
            r.summary(),
            "rollout-1  production  registry.example.invalid/ns/app:v1 -> registry.example.invalid/ns/app:v2  traffic 0%  state: pending  retained: slot-0"
        );
        r.fallback = Some(FallbackSupport::Native);
        assert_eq!(
            r.summary(),
            "rollout-1  production  registry.example.invalid/ns/app:v1 -> registry.example.invalid/ns/app:v2  traffic 0%  state: pending  retained: slot-0"
        );
        r.fallback = Some(FallbackSupport::BestEffort);
        assert_eq!(
            r.summary(),
            "rollout-1  production  registry.example.invalid/ns/app:v1 -> registry.example.invalid/ns/app:v2  traffic 0%  state: pending  retained: slot-0  fallback: best-effort"
        );
    }

    #[test]
    fn record_round_trips_through_json() {
        let mut r = record("production");
        r.state = parked(1);
        r.slot = Some(Slot::new("slot-1"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"parked\""));
        assert_eq!(serde_json::from_str::<RolloutRecord>(&json).unwrap(), r);
    }

    fn state_for(variant: u8, step: usize) -> RolloutState {
        match variant {
            0 => RolloutState::Pending,
            1 => RolloutState::Step(step),
            2 => RolloutState::Baking { step, since: t0() },
            3 => RolloutState::Complete,
            4 => RolloutState::RollingBack,
            5 => RolloutState::RolledBack,
            6 => RolloutState::Halted,
            _ => RolloutState::Parked {
                until: t0(),
                resume_state: Box::new(RolloutState::Step(step)),
            },
        }
    }

    fn arb_plan() -> impl proptest::strategy::Strategy<Value = DeployPlan> {
        prop::collection::btree_set(1u8..100, 0..9).prop_map(|set| {
            let mut steps: Vec<u8> = set.into_iter().collect();
            steps.push(100);
            let src = format!("[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"gradual\"\nsteps = {steps:?}\nmin_step_duration = \"1h\"\n");
            DeployTemplate::parse(&src).unwrap().plan("sandbox").unwrap()
        })
    }

    proptest! {
        #[test]
        fn applied_traffic_never_exceeds_the_current_step(plan in arb_plan(), variant in 0u8..8, raw_step in 0usize..10, held in 0u8..=100, percent in any::<u8>()) {
            let step = raw_step % plan.steps.len();
            let mut record = RolloutRecord::new("r", plan, "img:v2", "img:v1", t0());
            record.traffic_percent = held;
            record.state = state_for(variant, step);
            let ceiling = record.traffic_ceiling();
            let allowed = record.set_traffic(percent).is_ok();
            prop_assert_eq!(allowed, percent <= ceiling);
            prop_assert_eq!(record.traffic_percent, if allowed { percent } else { held });
            if let Some(n) = record.state.step() {
                prop_assert_eq!(ceiling, record.plan.steps[n].traffic_percent);
            }
            match record.state {
                RolloutState::Halted => prop_assert_eq!(ceiling, held),
                RolloutState::Complete => prop_assert_eq!(ceiling, 100),
                RolloutState::Pending | RolloutState::RollingBack | RolloutState::RolledBack => prop_assert_eq!(ceiling, 0),
                _ => {}
            }
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use crate::deploy::{DeployStep, Precondition, StepKind};

    #[kani::proof]
    #[kani::unwind(4)]
    fn set_traffic_never_exceeds_the_step() {
        let percents = [10u8, 50, 100];
        let steps: Vec<DeployStep> = percents
            .iter()
            .enumerate()
            .map(|(index, p)| DeployStep {
                index,
                kind: StepKind::Traffic,
                traffic_percent: *p,
                min_duration: chrono::Duration::zero(),
                bake_time: chrono::Duration::zero(),
                preconditions: vec![Precondition::HealthOk],
            })
            .collect();
        let plan = DeployPlan {
            environment: "sandbox".into(),
            image: "app".into(),
            risk_class: crate::deploy::RiskClass::Unused,
            strategy: crate::deploy::Strategy::Gradual,
            lease: "deploy:app:sandbox".into(),
            health: None,
            rollback: crate::deploy::Rollback {
                automatic: true,
                on_breach: crate::deploy::OnBreach::Rollback,
                retain_for: chrono::Duration::zero(),
            },
            shadow: None,
            steps,
        };
        let step: usize = kani::any();
        kani::assume(step < 3);
        let percent: u8 = kani::any();
        let mut record = RolloutRecord::new("r", plan, "app:v2", "app:v1", Utc::now());
        record.state = if kani::any() {
            RolloutState::Step(step)
        } else {
            RolloutState::Baking {
                step,
                since: Utc::now(),
            }
        };
        let _ = record.set_traffic(percent);
        assert!(record.traffic_percent <= percents[step]);
    }
}
