use super::template::{DeployTemplate, Health, RiskClass, Rollback, Shadow, Strategy};
use super::{is_production_env, repo_identity, DeployError};
use crate::leases::LeaseName;
use crate::windows::WindowSet;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

/// Something that must hold before a step may start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precondition {
    /// The named availability window must be open.
    WindowOpen(String),
    /// The health gates from `[health]` must pass.
    HealthOk,
    /// The named coordination lease must be held by the executor.
    LeaseHeld(String),
}

/// The kind of a [`Precondition`], without its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreconditionKind {
    /// [`Precondition::WindowOpen`].
    Window,
    /// [`Precondition::HealthOk`].
    Health,
    /// [`Precondition::LeaseHeld`].
    Lease,
}

impl PreconditionKind {
    /// Every kind.
    pub const ALL: [PreconditionKind; 3] = [
        PreconditionKind::Window,
        PreconditionKind::Health,
        PreconditionKind::Lease,
    ];

    /// Name used in output.
    pub const fn name(self) -> &'static str {
        match self {
            PreconditionKind::Window => "window",
            PreconditionKind::Health => "health",
            PreconditionKind::Lease => "lease",
        }
    }
}

/// Which precondition kinds the run path evaluates, which decides whether
/// plan output is labelled advisory.
///
/// ```
/// use harness::deploy::{Enforcement, PreconditionKind};
///
/// assert!(Enforcement::of(|_| true).is_complete());
/// let partial = Enforcement::of(|kind| kind != PreconditionKind::Lease);
/// assert!(!partial.is_complete());
/// assert_eq!(partial.unenforced(), [PreconditionKind::Lease]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enforcement {
    window: bool,
    health: bool,
    lease: bool,
}

impl Enforcement {
    /// Enforcement as decided by `enforced` for each kind.
    pub fn of(enforced: impl Fn(PreconditionKind) -> bool) -> Self {
        Self {
            window: enforced(PreconditionKind::Window),
            health: enforced(PreconditionKind::Health),
            lease: enforced(PreconditionKind::Lease),
        }
    }

    /// Whether `kind` is evaluated before each step.
    pub fn enforces(&self, kind: PreconditionKind) -> bool {
        match kind {
            PreconditionKind::Window => self.window,
            PreconditionKind::Health => self.health,
            PreconditionKind::Lease => self.lease,
        }
    }

    /// The kinds that are declared but not evaluated.
    pub fn unenforced(&self) -> Vec<PreconditionKind> {
        PreconditionKind::ALL
            .into_iter()
            .filter(|kind| !self.enforces(*kind))
            .collect()
    }

    /// True when every kind is evaluated, so output needs no advisory label.
    pub fn is_complete(&self) -> bool {
        self.unenforced().is_empty()
    }

    fn label(&self) -> Option<String> {
        let names = self
            .unenforced()
            .into_iter()
            .map(PreconditionKind::name)
            .collect::<Vec<_>>()
            .join(", ");
        (!self.is_complete())
            .then(|| format!("advisory only: {names} preconditions are declared, not enforced"))
    }
}

impl Precondition {
    /// The kind of this precondition.
    pub fn kind(&self) -> PreconditionKind {
        match self {
            Precondition::WindowOpen(_) => PreconditionKind::Window,
            Precondition::HealthOk => PreconditionKind::Health,
            Precondition::LeaseHeld(_) => PreconditionKind::Lease,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Precondition::WindowOpen(name) => json!({ "window_open": name }),
            Precondition::HealthOk => json!("health_ok"),
            Precondition::LeaseHeld(name) => json!({ "lease_held": name }),
        }
    }
}

impl fmt::Display for Precondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Precondition::WindowOpen(name) => write!(f, "window-open({name})"),
            Precondition::HealthOk => f.write_str("health-ok"),
            Precondition::LeaseHeld(name) => write!(f, "lease-held({name})"),
        }
    }
}

/// What a step does to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Route `traffic_percent` of live traffic to the new version.
    Traffic,
    /// Mirror `mirror_percent` of traffic to the new version; responses are discarded.
    Shadow {
        /// Share of traffic mirrored.
        mirror_percent: u8,
    },
    /// Atomically switch all traffic to the inactive slot.
    Swap,
    /// Retire the previous slot once `min_duration` has elapsed.
    Retire,
}

impl StepKind {
    /// Short name used in text and JSON output.
    pub const fn name(self) -> &'static str {
        match self {
            StepKind::Traffic => "traffic",
            StepKind::Shadow { .. } => "shadow",
            StepKind::Swap => "swap",
            StepKind::Retire => "retire",
        }
    }
}

/// One ordered step of a [`DeployPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployStep {
    /// Position in the plan, starting at 0.
    pub index: usize,
    /// What the step does.
    pub kind: StepKind,
    /// Share of live traffic served by the new version once the step is applied.
    pub traffic_percent: u8,
    /// Minimum time to hold the step before advancing.
    pub min_duration: Duration,
    /// Observation period during which health is polled after the step is applied.
    pub bake_time: Duration,
    /// Conditions that must hold before the step starts.
    pub preconditions: Vec<Precondition>,
}

impl DeployStep {
    fn to_json(&self) -> Value {
        let mut value = json!({
            "index": self.index,
            "kind": self.kind.name(),
            "traffic_percent": self.traffic_percent,
            "min_duration_seconds": self.min_duration.num_seconds(),
            "bake_time_seconds": self.bake_time.num_seconds(),
            "preconditions": self.preconditions.iter().map(Precondition::to_json).collect::<Vec<_>>(),
        });
        if let StepKind::Shadow { mirror_percent } = self.kind {
            value["mirror_percent"] = json!(mirror_percent);
        }
        value
    }

    fn label(&self) -> String {
        match self.kind {
            StepKind::Traffic => format!("traffic {}%", self.traffic_percent),
            StepKind::Shadow { mirror_percent } => format!("shadow {mirror_percent}%"),
            StepKind::Swap | StepKind::Retire => self.kind.name().to_string(),
        }
    }
}

/// The ordered steps a rollout executor performs for one environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeployPlan {
    /// Environment the plan targets.
    pub environment: String,
    /// Fully qualified image reference being rolled out.
    pub image: String,
    /// Effective risk class the plan was produced under.
    pub risk_class: RiskClass,
    /// Strategy the steps implement.
    pub strategy: Strategy,
    /// Coordination lease every step requires, `deploy:<repo>/<image>:<env>`.
    pub lease: String,
    /// Health gates polled while a step bakes, when the template has `[health]`.
    pub health: Option<Health>,
    /// What a health breach triggers.
    pub rollback: Rollback,
    /// The template's `[shadow]` section when mirroring is enabled: what a
    /// `Shadow` step compares and the divergence rate that breaches.
    pub shadow: Option<Shadow>,
    /// Steps in execution order.
    pub steps: Vec<DeployStep>,
}

impl DeployPlan {
    /// The lease name guarding deployments of `image` from `repo` to `env`:
    /// `deploy:<repo>/<image>:<env>`, built by [`LeaseName::deploy_image`] so
    /// it is the same key [`required_leases`](crate::leases::required_leases)
    /// produces for a production effect.
    pub fn lease_name(repo: &str, image: &str, env: &str) -> String {
        LeaseName::deploy_image(repo, image, env).to_string()
    }

    /// Sum of every step's hold and bake time.
    pub fn total_min_duration(&self) -> Duration {
        self.steps.iter().fold(Duration::zero(), |acc, s| {
            acc + s.min_duration + s.bake_time
        })
    }

    /// Human-readable rendering, one line per step.
    ///
    /// ```
    /// use harness::deploy::DeployTemplate;
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"staging\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"blue-green\"\nmin_step_duration = \"1h\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n",
    /// )
    /// .unwrap();
    /// let text = template.plan("staging").unwrap().render_text();
    /// assert!(text.contains("1. swap"));
    /// assert!(text.contains("2. retire         hold 2d"));
    /// ```
    pub fn render_text(&self) -> String {
        self.render_text_with(&crate::rollout::enforcement())
    }

    /// [`DeployPlan::render_text`] for a given [`Enforcement`]: the advisory
    /// line appears exactly when some precondition kind is not enforced.
    pub fn render_text_with(&self, enforcement: &Enforcement) -> String {
        let mut out = format!(
            "deploy plan: {} -> {}\nrisk class: {} | strategy: {} | lease: {}\n",
            self.image, self.environment, self.risk_class, self.strategy, self.lease
        );
        if let Some(label) = enforcement.label() {
            out.push_str(&label);
            out.push('\n');
        }
        for step in &self.steps {
            let requires = step
                .preconditions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "{:3}. {:<14} hold {:<6} bake {:<6} requires: {requires}\n",
                step.index + 1,
                step.label(),
                format_duration(step.min_duration),
                format_duration(step.bake_time)
            ));
        }
        out.push_str(&format!(
            "minimum total: {}\n",
            format_duration(self.total_min_duration())
        ));
        out
    }

    /// JSON rendering; durations are in seconds.
    ///
    /// ```
    /// use harness::deploy::DeployTemplate;
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
    /// )
    /// .unwrap();
    /// let json = template.plan("sandbox").unwrap().to_json();
    /// assert_eq!(json["strategy"], "instant");
    /// assert_eq!(json["steps"][0]["traffic_percent"], 100);
    /// assert_eq!(json["steps"][0]["preconditions"][0]["lease_held"], "deploy:registry.example.invalid/ns/app:sandbox");
    /// ```
    pub fn to_json(&self) -> Value {
        self.to_json_with(&crate::rollout::enforcement())
    }

    /// [`DeployPlan::to_json`] for a given [`Enforcement`]: `advisory` and
    /// `unenforced` are present exactly when some precondition kind is not
    /// enforced; `enforcement` always reports each kind.
    pub fn to_json_with(&self, enforcement: &Enforcement) -> Value {
        let mut value = json!({
            "enforcement": {
                "window": enforcement.enforces(PreconditionKind::Window),
                "health": enforcement.enforces(PreconditionKind::Health),
                "lease": enforcement.enforces(PreconditionKind::Lease),
            },
            "environment": self.environment,
            "image": self.image,
            "risk_class": self.risk_class.name(),
            "strategy": self.strategy.name(),
            "lease": self.lease,
            "on_breach": self.rollback.on_breach.name(),
            "total_min_duration_seconds": self.total_min_duration().num_seconds(),
            "steps": self.steps.iter().map(DeployStep::to_json).collect::<Vec<_>>(),
        });
        if !enforcement.is_complete() {
            value["advisory"] = json!(true);
            value["unenforced"] = json!(enforcement
                .unenforced()
                .into_iter()
                .map(PreconditionKind::name)
                .collect::<Vec<_>>());
        }
        if let Some(shadow) = &self.shadow {
            value["shadow"] = json!({
                "mirror_percent": shadow.mirror_percent,
                "compare": shadow.compare.iter().map(|c| c.name()).collect::<Vec<_>>(),
                "max_divergence": shadow.max_divergence,
            });
        }
        value
    }

    /// Pretty-printed [`DeployPlan::to_json`] for the CLI.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.to_json()).expect("a JSON value serialises")
    }
}

/// Load `<repo>/.nanna/deploy.toml` and plan a rollout to `env`.
///
/// `score` resolves a derived risk class; a static class ignores it. This
/// does not check `rollout.windows` against a real [`WindowSet`]: a template
/// naming a window that does not exist anywhere is accepted. Prefer
/// [`plan_for_repo_checked`], which validates against the host's windows.
pub fn plan_for_repo(
    repo: &Path,
    env: &str,
    score: Option<u32>,
) -> Result<DeployPlan, DeployError> {
    let template = DeployTemplate::load_from_repo(repo)?;
    let identity = repo_identity(repo).unwrap_or_else(|| template.target.registry.clone());
    template.plan_for(&identity, env, score)
}

/// Load `<repo>/.nanna/deploy.toml`, validate `rollout.windows` against the
/// host's windows file ([`host_windows`]) and plan a rollout.
///
/// The windows are read from `$NANNA_CONFIG_DIR` (else the XDG config
/// directory), never from the repository: a `<repo>/.nanna/windows.toml` is
/// ignored. A production plan that names a window while the host has no
/// windows file is refused.
///
/// ```
/// use harness::deploy::{plan_for_repo_with_windows, DeployError};
/// use harness::windows::WindowSet;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
/// std::fs::write(
///     dir.path().join(".nanna/deploy.toml"),
///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\nwindows = \"not-a-real-window\"\n",
/// )
/// .unwrap();
/// let host = WindowSet::parse(
///     "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n",
/// )
/// .unwrap();
/// let err = plan_for_repo_with_windows(dir.path(), "sandbox", None, Some(&host)).unwrap_err();
/// assert!(matches!(err, DeployError::InvalidField { field: "rollout.windows", .. }));
/// ```
pub fn plan_for_repo_checked(
    repo: &Path,
    env: &str,
    score: Option<u32>,
) -> Result<DeployPlan, DeployError> {
    plan_for_repo_checked_from(repo, env, score, &|key: &str| -> Option<OsString> {
        std::env::var_os(key)
    })
}

pub(super) fn plan_for_repo_checked_from(
    repo: &Path,
    env: &str,
    score: Option<u32>,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Result<DeployPlan, DeployError> {
    let windows = super::host_windows_from(lookup)?;
    plan_for_repo_with_windows(repo, env, score, windows.as_ref())
}

/// [`plan_for_repo_checked`] against an already loaded host window set.
/// `None` means the host has no windows: a production plan naming a window
/// is then refused with [`DeployError::HostWindowsMissing`].
pub fn plan_for_repo_with_windows(
    repo: &Path,
    env: &str,
    score: Option<u32>,
    windows: Option<&WindowSet>,
) -> Result<DeployPlan, DeployError> {
    let template = DeployTemplate::load_from_repo(repo)?;
    match (windows, template.rollout.windows.as_ref()) {
        (Some(windows), _) => template.validate_against(windows)?,
        (None, Some(window)) if is_production_env(env) => {
            return Err(DeployError::HostWindowsMissing {
                window: window.clone(),
            })
        }
        (None, _) => {}
    }
    let identity = repo_identity(repo).unwrap_or_else(|| template.target.registry.clone());
    template.plan_for(&identity, env, score)
}

impl fmt::Display for DeployPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render_text())
    }
}

/// Render a duration as days, hours and minutes, e.g. `1d 4h 30m`.
pub fn format_duration(duration: Duration) -> String {
    let minutes = duration.num_minutes();
    let parts = [
        (minutes / 1_440, "d"),
        (minutes % 1_440 / 60, "h"),
        (minutes % 60, "m"),
    ];
    let text = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return "0m".to_string();
    }
    text
}

impl DeployTemplate {
    /// Build the plan for `env` under the template's static risk class.
    ///
    /// Every step requires the deploy lease; production steps also require
    /// the template's window; steps require health whenever `[health]` is
    /// present. A derived class needs [`DeployTemplate::plan_with_score`].
    ///
    /// ```
    /// use harness::deploy::{DeployTemplate, Precondition, StepKind};
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"production\"]\n[risk]\nclass = \"edge\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [10, 50, 100]\nmin_step_duration = \"8h\"\nwindows = \"business-hours\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n",
    /// )
    /// .unwrap();
    /// let plan = template.plan("production").unwrap();
    /// let percents: Vec<u8> = plan.steps.iter().map(|s| s.traffic_percent).collect();
    /// assert_eq!(percents, [10, 50, 100]);
    /// assert!(plan.steps.iter().all(|s| s.kind == StepKind::Traffic));
    /// assert_eq!(
    ///     plan.steps[0].preconditions,
    ///     [
    ///         Precondition::LeaseHeld("deploy:registry.example.invalid/ns/app:production".into()),
    ///         Precondition::WindowOpen("business-hours".into()),
    ///         Precondition::HealthOk,
    ///     ]
    /// );
    /// ```
    pub fn plan(&self, env: &str) -> Result<DeployPlan, DeployError> {
        self.plan_with_score(env, None)
    }

    /// Build the plan for `env`, resolving a derived risk class from `score`.
    ///
    /// ```
    /// use harness::deploy::{DeployTemplate, RiskClass, StepKind};
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"staging\"]\n[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = 50\n[rollout]\nstrategy = \"shadow-then-gradual\"\nsteps = [10, 50, 100]\nmin_step_duration = \"8h\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"10m\"\n[shadow]\nenabled = true\nmirror_percent = 5\ncompare = [\"status\", \"latency\"]\n",
    /// )
    /// .unwrap();
    /// let plan = template.plan_with_score("staging", Some(75)).unwrap();
    /// assert_eq!(plan.risk_class, RiskClass::Edge);
    /// assert_eq!(plan.steps[0].kind, StepKind::Shadow { mirror_percent: 5 });
    /// assert_eq!(plan.steps[0].traffic_percent, 0);
    /// assert_eq!(plan.steps[3].traffic_percent, 100);
    /// ```
    pub fn plan_with_score(
        &self,
        env: &str,
        score: Option<u32>,
    ) -> Result<DeployPlan, DeployError> {
        self.plan_for(&self.target.registry, env, score)
    }

    /// Build the plan for `env` with the deploy lease keyed by `repo` (an
    /// `owner/name` identity) and the template's image, so repositories that
    /// share an image name do not share a lease. [`DeployTemplate::plan`]
    /// uses the target registry as `repo` when no repository is known.
    ///
    /// ```
    /// use harness::deploy::DeployTemplate;
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
    /// )
    /// .unwrap();
    /// let plan = template.plan_for("example/repo", "sandbox", None).unwrap();
    /// assert_eq!(plan.lease, "deploy:example/repo/app:sandbox");
    /// ```
    pub fn plan_for(
        &self,
        repo: &str,
        env: &str,
        score: Option<u32>,
    ) -> Result<DeployPlan, DeployError> {
        self.validate()?;
        if !self.target.environments.iter().any(|e| e == env) {
            return Err(DeployError::UnknownEnvironment {
                env: env.to_string(),
                known: self.target.environments.clone(),
            });
        }
        let risk_class = self.resolve_risk(score)?;
        let lease = DeployPlan::lease_name(repo, &self.target.image, env);
        let mut preconditions = vec![Precondition::LeaseHeld(lease.clone())];
        if is_production_env(env) {
            preconditions.extend(self.rollout.windows.clone().map(Precondition::WindowOpen));
        }
        if self.health.is_some() {
            preconditions.push(Precondition::HealthOk);
        }
        let bake_time = self
            .health
            .as_ref()
            .map_or_else(Duration::zero, |h| h.bake_time);
        let hold = self.rollout.min_step_duration;
        let step = |kind, traffic_percent, min_duration, bake_time| DeployStep {
            index: 0,
            kind,
            traffic_percent,
            min_duration,
            bake_time,
            preconditions: preconditions.clone(),
        };
        let mut steps = Vec::new();
        match self.rollout.strategy {
            Strategy::BlueGreen => {
                steps.push(step(StepKind::Swap, 100, hold, bake_time));
                steps.push(step(
                    StepKind::Retire,
                    100,
                    self.rollback.retain_for,
                    Duration::zero(),
                ));
            }
            Strategy::Instant | Strategy::Gradual | Strategy::ShadowThenGradual => {
                if let Some(shadow) = self.shadow.as_ref().filter(|s| s.enabled) {
                    steps.push(step(
                        StepKind::Shadow {
                            mirror_percent: shadow.mirror_percent,
                        },
                        0,
                        hold,
                        bake_time,
                    ));
                }
                steps.extend(
                    self.rollout
                        .steps
                        .iter()
                        .map(|p| step(StepKind::Traffic, *p, hold, bake_time)),
                );
            }
        }
        for (index, step) in steps.iter_mut().enumerate() {
            step.index = index;
        }
        Ok(DeployPlan {
            environment: env.to_string(),
            image: self.target.image_ref(),
            risk_class,
            strategy: self.rollout.strategy,
            lease,
            health: self.health.clone(),
            rollback: self.rollback.clone(),
            shadow: self.shadow.clone().filter(|s| s.enabled),
            steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::template::tests::FIXTURE;
    use super::super::template::OnBreach;
    use super::super::validate::{min_span, min_steps, strategy_allowed};
    use super::*;
    use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest, Strategy as _};

    const HEAD: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\", \"production\"]\n";
    const HEALTH: &str = "[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n";

    fn template(
        class: &str,
        strategy: &str,
        steps: &str,
        min_step: &str,
        tail: &str,
    ) -> DeployTemplate {
        let src = format!("{HEAD}[risk]\nclass = \"{class}\"\n[rollout]\nstrategy = \"{strategy}\"\nsteps = {steps}\nmin_step_duration = \"{min_step}\"\nwindows = \"business-hours\"\n{HEALTH}{tail}");
        DeployTemplate::parse(&src).unwrap()
    }

    fn kinds(plan: &DeployPlan) -> Vec<StepKind> {
        plan.steps.iter().map(|s| s.kind).collect()
    }

    fn percents(plan: &DeployPlan) -> Vec<u8> {
        plan.steps.iter().map(|s| s.traffic_percent).collect()
    }

    #[test]
    fn gradual_plan_follows_the_steps() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("production")
            .unwrap();
        assert_eq!(plan.environment, "production");
        assert_eq!(plan.image, "registry.example.invalid/ns/fullstack-fixture");
        assert_eq!(plan.risk_class, RiskClass::Edge);
        assert_eq!(plan.strategy, Strategy::Gradual);
        assert_eq!(
            plan.lease,
            "deploy:registry.example.invalid/ns/fullstack-fixture:production"
        );
        assert_eq!(percents(&plan), [10, 50, 100]);
        assert_eq!(kinds(&plan), [StepKind::Traffic; 3]);
        assert_eq!(
            plan.steps.iter().map(|s| s.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        for step in &plan.steps {
            assert_eq!(step.min_duration, Duration::hours(8));
            assert_eq!(step.bake_time, Duration::minutes(30));
            assert_eq!(
                step.preconditions,
                [
                    Precondition::LeaseHeld(
                        "deploy:registry.example.invalid/ns/fullstack-fixture:production".into()
                    ),
                    Precondition::WindowOpen("business-hours".into()),
                    Precondition::HealthOk
                ]
            );
        }
        assert_eq!(
            plan.total_min_duration(),
            Duration::hours(25) + Duration::minutes(30)
        );
    }

    #[test]
    fn plan_round_trips_through_serde_and_carries_health_and_rollback() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("production")
            .unwrap();
        let health = plan.health.as_ref().unwrap();
        assert_eq!(health.endpoints, ["/health/v1"]);
        assert_eq!(health.latency_p99_max_ms, 800);
        assert_eq!(plan.rollback.on_breach, OnBreach::Rollback);
        assert!(plan.rollback.automatic);
        assert_eq!(plan.to_json()["on_breach"], "rollback");
        let json = serde_json::to_string(&plan).unwrap();
        let back: DeployPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plan);
        assert!(json.contains("\"window_open\":\"business-hours\""));
        assert!(json.contains("\"health_ok\""));
        let shadow = template(
            "edge",
            "shadow-then-gradual",
            "[10, 50, 100]",
            "8h",
            "[shadow]\nenabled = true\nmirror_percent = 5\ncompare = [\"status\"]\n",
        )
        .plan("production")
        .unwrap();
        let json = serde_json::to_string(&shadow).unwrap();
        assert!(json.contains("\"kind\":{\"shadow\":{\"mirror_percent\":5}}"));
        assert_eq!(serde_json::from_str::<DeployPlan>(&json).unwrap(), shadow);
    }

    #[test]
    fn non_production_environments_skip_the_window() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("staging")
            .unwrap();
        assert_eq!(
            plan.lease,
            "deploy:registry.example.invalid/ns/fullstack-fixture:staging"
        );
        for step in &plan.steps {
            assert_eq!(
                step.preconditions,
                [
                    Precondition::LeaseHeld(
                        "deploy:registry.example.invalid/ns/fullstack-fixture:staging".into()
                    ),
                    Precondition::HealthOk
                ]
            );
        }
    }

    #[test]
    fn instant_plan_is_a_single_full_step() {
        let src = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"r.invalid\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n";
        let plan = DeployTemplate::parse(src).unwrap().plan("sandbox").unwrap();
        assert_eq!(kinds(&plan), [StepKind::Traffic]);
        assert_eq!(percents(&plan), [100]);
        assert_eq!(plan.steps[0].min_duration, Duration::zero());
        assert_eq!(plan.steps[0].bake_time, Duration::zero());
        assert_eq!(
            plan.steps[0].preconditions,
            [Precondition::LeaseHeld(
                "deploy:r.invalid/app:sandbox".into()
            )]
        );
        assert_eq!(plan.total_min_duration(), Duration::zero());
    }

    #[test]
    fn blue_green_plan_swaps_then_retires() {
        let t = template(
            "internal",
            "blue-green",
            "[100]",
            "1h",
            "[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n",
        );
        let plan = t.plan("production").unwrap();
        assert_eq!(kinds(&plan), [StepKind::Swap, StepKind::Retire]);
        assert_eq!(percents(&plan), [100, 100]);
        assert_eq!(plan.steps[0].min_duration, Duration::hours(1));
        assert_eq!(plan.steps[0].bake_time, Duration::minutes(30));
        assert_eq!(plan.steps[1].min_duration, Duration::days(2));
        assert_eq!(plan.steps[1].bake_time, Duration::zero());
        assert_eq!(plan.steps[1].index, 1);
        assert_eq!(plan.steps[1].preconditions, plan.steps[0].preconditions);
    }

    #[test]
    fn shadow_then_gradual_plan_mirrors_first() {
        let t = template(
            "core",
            "shadow-then-gradual",
            "[1, 5, 10, 25, 50, 75, 100]",
            "1d",
            "[shadow]\nenabled = true\nmirror_percent = 15\ncompare = [\"status\", \"latency\"]\n",
        );
        let plan = t.plan("production").unwrap();
        assert_eq!(plan.steps.len(), 8);
        assert_eq!(plan.steps[0].kind, StepKind::Shadow { mirror_percent: 15 });
        assert_eq!(plan.steps[0].traffic_percent, 0);
        assert_eq!(plan.steps[0].min_duration, Duration::days(1));
        assert_eq!(plan.steps[0].bake_time, Duration::minutes(30));
        assert_eq!(percents(&plan)[1..], [1, 5, 10, 25, 50, 75, 100]);
        assert!(plan.steps[1..].iter().all(|s| s.kind == StepKind::Traffic));
        assert_eq!(
            plan.total_min_duration(),
            Duration::days(8) + Duration::hours(4)
        );
    }

    #[test]
    fn unknown_environment_is_rejected() {
        let err = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("prod")
            .unwrap_err();
        match err {
            DeployError::UnknownEnvironment { env, known } => {
                assert_eq!(env, "prod");
                assert_eq!(known, ["sandbox", "staging", "production"]);
            }
            other => panic!("expected UnknownEnvironment, got {other:?}"),
        }
        assert!(DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("prod")
            .unwrap_err()
            .to_string()
            .contains("sandbox, staging, production"));
    }

    #[test]
    fn derived_plan_needs_a_score() {
        let src = FIXTURE.replace(
            "[risk]\nclass = \"edge\"\n",
            "[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = 40\n",
        );
        let t = DeployTemplate::parse(&src).unwrap();
        assert!(matches!(
            t.plan("production"),
            Err(DeployError::ScoreRequired)
        ));
        assert_eq!(
            t.plan_with_score("production", Some(3)).unwrap().risk_class,
            RiskClass::Unused
        );
        assert_eq!(
            t.plan_with_score("production", Some(40))
                .unwrap()
                .risk_class,
            RiskClass::Edge
        );
        assert_eq!(
            DeployTemplate::parse(FIXTURE)
                .unwrap()
                .plan_with_score("production", Some(9_999))
                .unwrap()
                .risk_class,
            RiskClass::Edge
        );
    }

    #[test]
    fn lease_names_follow_the_documented_form() {
        assert_eq!(
            DeployPlan::lease_name("example/repo", "app", "production"),
            "deploy:example/repo/app:production"
        );
    }

    #[test]
    fn renders_text() {
        let text = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("production")
            .unwrap()
            .render_text();
        let expected = "\
deploy plan: registry.example.invalid/ns/fullstack-fixture -> production
risk class: edge | strategy: gradual | lease: deploy:registry.example.invalid/ns/fullstack-fixture:production
  1. traffic 10%    hold 8h     bake 30m    requires: lease-held(deploy:registry.example.invalid/ns/fullstack-fixture:production), window-open(business-hours), health-ok
  2. traffic 50%    hold 8h     bake 30m    requires: lease-held(deploy:registry.example.invalid/ns/fullstack-fixture:production), window-open(business-hours), health-ok
  3. traffic 100%   hold 8h     bake 30m    requires: lease-held(deploy:registry.example.invalid/ns/fullstack-fixture:production), window-open(business-hours), health-ok
minimum total: 1d 1h 30m
";
        assert_eq!(text, expected);
        assert_eq!(
            DeployTemplate::parse(FIXTURE)
                .unwrap()
                .plan("production")
                .unwrap()
                .to_string(),
            expected
        );
    }

    #[test]
    fn renders_every_step_kind() {
        let bg = template(
            "internal",
            "blue-green",
            "[100]",
            "1h",
            "[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n",
        )
        .plan("sandbox")
        .unwrap()
        .render_text();
        assert!(bg.contains("  1. swap           hold 1h     bake 30m    requires: lease-held(deploy:registry.example.invalid/ns/app:sandbox), health-ok\n"), "{bg}");
        assert!(bg.contains("  2. retire         hold 2d     bake 0m     requires: lease-held(deploy:registry.example.invalid/ns/app:sandbox), health-ok\n"), "{bg}");
        let shadow = template(
            "unused",
            "shadow-then-gradual",
            "[100]",
            "0m",
            "[shadow]\nenabled = true\nmirror_percent = 7\ncompare = [\"status\"]\n",
        )
        .plan("sandbox")
        .unwrap()
        .render_text();
        assert!(shadow.contains("  1. shadow 7%      hold 0m     bake 30m    requires: lease-held(deploy:registry.example.invalid/ns/app:sandbox), health-ok\n"), "{shadow}");
        assert!(shadow.ends_with("minimum total: 1h\n"), "{shadow}");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::zero()), "0m");
        assert_eq!(format_duration(Duration::minutes(30)), "30m");
        assert_eq!(format_duration(Duration::hours(8)), "8h");
        assert_eq!(format_duration(Duration::days(2)), "2d");
        assert_eq!(
            format_duration(Duration::days(1) + Duration::minutes(5)),
            "1d 5m"
        );
        assert_eq!(
            format_duration(Duration::hours(25) + Duration::minutes(30)),
            "1d 1h 30m"
        );
    }

    #[test]
    fn json_carries_every_field() {
        let t = template(
            "core",
            "shadow-then-gradual",
            "[1, 5, 10, 25, 50, 75, 100]",
            "1d",
            "[shadow]\nenabled = true\nmirror_percent = 15\ncompare = [\"status\", \"latency\"]\n",
        );
        let json = t.plan("production").unwrap().to_json();
        assert!(json.get("advisory").is_none());
        assert_eq!(
            json["enforcement"],
            serde_json::json!({"window": true, "health": true, "lease": true})
        );
        assert_eq!(json["environment"], "production");
        assert_eq!(json["image"], "registry.example.invalid/ns/app");
        assert_eq!(json["risk_class"], "core");
        assert_eq!(json["strategy"], "shadow-then-gradual");
        assert_eq!(
            json["lease"],
            "deploy:registry.example.invalid/ns/app:production"
        );
        assert_eq!(json["total_min_duration_seconds"], 8 * 86_400 + 4 * 3_600);
        let steps = json["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 8);
        assert_eq!(steps[0]["kind"], "shadow");
        assert_eq!(steps[0]["mirror_percent"], 15);
        assert_eq!(steps[0]["traffic_percent"], 0);
        assert_eq!(steps[1]["kind"], "traffic");
        assert!(steps[1].get("mirror_percent").is_none());
        assert_eq!(steps[1]["index"], 1);
        assert_eq!(steps[1]["traffic_percent"], 1);
        assert_eq!(steps[1]["min_duration_seconds"], 86_400);
        assert_eq!(steps[1]["bake_time_seconds"], 1_800);
        assert_eq!(
            steps[1]["preconditions"],
            serde_json::json!([{"lease_held": "deploy:registry.example.invalid/ns/app:production"}, {"window_open": "business-hours"}, "health_ok"])
        );
        let bg = template(
            "internal",
            "blue-green",
            "[100]",
            "1h",
            "[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"2d\"\n",
        )
        .plan("sandbox")
        .unwrap()
        .to_json();
        assert_eq!(bg["steps"][0]["kind"], "swap");
        assert_eq!(bg["steps"][1]["kind"], "retire");
    }

    #[test]
    fn plans_from_a_repo_checkout() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".nanna")).unwrap();
        std::fs::write(repo.path().join(".nanna/deploy.toml"), FIXTURE).unwrap();
        let plan = plan_for_repo(repo.path(), "staging", None).unwrap();
        assert_eq!(plan.environment, "staging");
        assert!(matches!(
            plan_for_repo(repo.path(), "prod", None),
            Err(DeployError::UnknownEnvironment { .. })
        ));
        assert!(matches!(
            plan_for_repo(&repo.path().join("missing"), "staging", None),
            Err(DeployError::Io { .. })
        ));
        let pretty = plan.to_json_pretty();
        assert!(pretty.starts_with("{\n  \"enforcement\": {"), "{pretty}");
        assert_eq!(
            serde_json::from_str::<Value>(&pretty).unwrap(),
            plan.to_json()
        );
    }

    fn repo_with_fixture() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".nanna")).unwrap();
        std::fs::write(repo.path().join(".nanna/deploy.toml"), FIXTURE).unwrap();
        repo
    }

    fn host_windows(names: &[&str]) -> WindowSet {
        let src = names
            .iter()
            .map(|name| format!("[[window]]\nname = \"{name}\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n"))
            .collect::<String>();
        WindowSet::parse(&src).unwrap()
    }

    #[test]
    fn checked_accepts_a_window_the_host_defines() {
        let repo = repo_with_fixture();
        let host = host_windows(&["business-hours"]);
        let plan =
            plan_for_repo_with_windows(repo.path(), "production", None, Some(&host)).unwrap();
        assert_eq!(plan.environment, "production");
    }

    #[test]
    fn checked_rejects_a_window_the_host_does_not_define() {
        let repo = repo_with_fixture();
        let host = host_windows(&["after-hours"]);
        assert!(matches!(
            plan_for_repo_with_windows(repo.path(), "staging", None, Some(&host)),
            Err(DeployError::InvalidField {
                field: "rollout.windows",
                ..
            })
        ));
    }

    #[test]
    fn production_without_host_windows_fails_closed() {
        let repo = repo_with_fixture();
        let err = plan_for_repo_with_windows(repo.path(), "production", None, None).unwrap_err();
        assert!(
            matches!(&err, DeployError::HostWindowsMissing { window } if window == "business-hours"),
            "{err:?}"
        );
        assert!(err.to_string().contains("NANNA_CONFIG_DIR"), "{err}");
    }

    #[test]
    fn non_production_without_host_windows_still_plans() {
        let repo = repo_with_fixture();
        let plan = plan_for_repo_with_windows(repo.path(), "staging", None, None).unwrap();
        assert_eq!(plan.environment, "staging");
    }

    #[test]
    fn a_repo_local_windows_file_is_ignored() {
        let repo = repo_with_fixture();
        std::fs::write(
            repo.path().join(".nanna/windows.toml"),
            "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"00:00\"\nend = \"23:59\"\napplies_to = [\"production\"]\n",
        )
        .unwrap();
        let none = |_: &str| -> Option<std::ffi::OsString> { None };
        assert!(matches!(
            plan_for_repo_checked_from(repo.path(), "production", None, &none),
            Err(DeployError::HostWindowsMissing { .. })
        ));
    }

    #[test]
    fn checked_reads_windows_from_the_host_config_dir() {
        let repo = repo_with_fixture();
        let config = tempfile::tempdir().unwrap();
        let lookup = |key: &str| -> Option<std::ffi::OsString> {
            (key == "NANNA_CONFIG_DIR").then(|| config.path().into())
        };
        assert!(matches!(
            plan_for_repo_checked_from(repo.path(), "production", None, &lookup),
            Err(DeployError::HostWindowsMissing { .. })
        ));
        std::fs::write(
            config.path().join("windows.toml"),
            "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n",
        )
        .unwrap();
        let plan = plan_for_repo_checked_from(repo.path(), "production", None, &lookup).unwrap();
        assert_eq!(plan.environment, "production");
    }

    #[test]
    fn checked_reports_a_malformed_host_windows_file() {
        let repo = repo_with_fixture();
        let config = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join("windows.toml"), "not valid toml [[[").unwrap();
        let lookup = |key: &str| -> Option<std::ffi::OsString> {
            (key == "NANNA_CONFIG_DIR").then(|| config.path().into())
        };
        let err = plan_for_repo_checked_from(repo.path(), "staging", None, &lookup).unwrap_err();
        assert!(matches!(err, DeployError::WindowSet { .. }), "{err:?}");
        assert!(err.to_string().contains("windows.toml"), "{err}");
    }

    #[test]
    #[serial_test::serial(nanna_config_dir_env)]
    fn plan_for_repo_checked_reads_windows_from_nanna_config_dir() {
        let repo = repo_with_fixture();
        let config = tempfile::tempdir().unwrap();
        struct RestoreEnv(Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("NANNA_CONFIG_DIR", v),
                    None => std::env::remove_var("NANNA_CONFIG_DIR"),
                }
            }
        }
        let _restore = RestoreEnv(std::env::var_os("NANNA_CONFIG_DIR"));
        std::env::set_var("NANNA_CONFIG_DIR", config.path());
        let window = |name: &str| {
            format!("[[window]]\nname = \"{name}\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n")
        };
        let missing = plan_for_repo_checked(repo.path(), "production", None);
        std::fs::write(config.path().join("windows.toml"), window("after-hours")).unwrap();
        let wrong = plan_for_repo_checked(repo.path(), "staging", None);
        std::fs::write(config.path().join("windows.toml"), window("business-hours")).unwrap();
        let right = plan_for_repo_checked(repo.path(), "production", None);
        let loaded = crate::deploy::host_windows().unwrap();
        assert!(matches!(
            missing,
            Err(DeployError::HostWindowsMissing { .. })
        ));
        assert!(matches!(
            wrong,
            Err(DeployError::InvalidField {
                field: "rollout.windows",
                ..
            })
        ));
        assert_eq!(right.unwrap().environment, "production");
        assert!(loaded.unwrap().window("business-hours").is_some());
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn the_lease_is_keyed_by_repo_and_image_not_the_image_basename() {
        let template = DeployTemplate::parse(FIXTURE).unwrap();
        let one = template.plan_for("example/one", "staging", None).unwrap();
        let two = template.plan_for("example/two", "staging", None).unwrap();
        let again = template.plan_for("example/one", "staging", None).unwrap();
        assert_eq!(one.lease, "deploy:example/one/fullstack-fixture:staging");
        assert_ne!(one.lease, two.lease);
        assert_eq!(one.lease, again.lease);
        let store = crate::leases::InMemoryLeaseStore::default();
        let lease = |plan: &DeployPlan| {
            let Precondition::LeaseHeld(name) = &plan.steps[0].preconditions[0] else {
                panic!("first precondition is the lease");
            };
            assert_eq!(*name, plan.lease);
            crate::rollout::RolloutRecord::new("r", plan.clone(), "i:2", "i:1", chrono::Utc::now())
                .lease_name()
                .unwrap()
        };
        let now = chrono::Utc::now();
        let ttl = Duration::hours(1);
        use crate::leases::LeaseStore;
        store.acquire(&lease(&one), "a", ttl, now).unwrap();
        assert!(store.acquire(&lease(&again), "b", ttl, now).is_err());
        assert!(store.acquire(&lease(&two), "b", ttl, now).is_ok());
    }

    #[test]
    fn the_plan_lease_is_the_one_the_production_effect_requires() {
        let template = DeployTemplate::parse(FIXTURE).unwrap();
        let plan = template
            .plan_for("example/one", "production", None)
            .unwrap();
        let ctx = crate::leases::LeaseContext {
            repo: "example/one",
            environment: Some("production"),
            image: Some("fullstack-fixture"),
            ..crate::leases::LeaseContext::default()
        };
        let required =
            crate::leases::required_leases(crate::leases::Effect::Production, &ctx).unwrap();
        assert_eq!(required[0].to_string(), plan.lease);
    }

    #[test]
    fn plans_from_a_repo_use_its_origin_identity_else_the_registry() {
        let repo = repo_with_fixture();
        let plan = plan_for_repo(repo.path(), "staging", None).unwrap();
        assert_eq!(
            plan.lease,
            "deploy:registry.example.invalid/ns/fullstack-fixture:staging"
        );
        git(repo.path(), &["init", "-q"]);
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@example.invalid:Org/service.git",
            ],
        );
        let plan = plan_for_repo(repo.path(), "staging", None).unwrap();
        assert_eq!(plan.lease, "deploy:Org/service/fullstack-fixture:staging");
        let host = host_windows(&["business-hours"]);
        let checked =
            plan_for_repo_with_windows(repo.path(), "staging", None, Some(&host)).unwrap();
        assert_eq!(checked.lease, plan.lease);
    }

    #[test]
    fn the_advisory_label_tracks_exactly_what_is_enforced() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("production")
            .unwrap();
        for mask in 0u8..8 {
            let enforcement = Enforcement::of(|kind| {
                mask & (1
                    << PreconditionKind::ALL
                        .iter()
                        .position(|k| *k == kind)
                        .unwrap())
                    != 0
            });
            let text = plan.render_text_with(&enforcement);
            let json = plan.to_json_with(&enforcement);
            let all = mask == 7;
            assert_eq!(enforcement.is_complete(), all);
            assert_eq!(text.contains("advisory only"), !all, "{mask}: {text}");
            assert_eq!(json.get("advisory").is_some(), !all, "{mask}: {json}");
            assert_eq!(json.get("unenforced").is_some(), !all, "{mask}: {json}");
            for kind in PreconditionKind::ALL {
                let named = enforcement.unenforced().contains(&kind);
                assert_eq!(json["enforcement"][kind.name()], !named);
                if !all {
                    assert_eq!(text.lines().nth(2).unwrap().contains(kind.name()), named);
                }
            }
        }
    }

    #[test]
    fn the_executor_enforces_every_precondition_kind_so_output_is_not_advisory() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("production")
            .unwrap();
        assert!(crate::rollout::enforcement().is_complete());
        assert!(!plan.render_text().contains("advisory"));
        assert!(plan.to_json().get("advisory").is_none());
        for kind in PreconditionKind::ALL {
            assert!(crate::rollout::RolloutExecutor::evaluates(kind));
        }
        for precondition in &plan.steps[0].preconditions {
            assert!(crate::rollout::enforcement().enforces(precondition.kind()));
        }
    }

    #[test]
    fn production_gates_apply_to_any_casing() {
        let src = FIXTURE.replace("\"production\"", "\"Production\"");
        let plan = DeployTemplate::parse(&src)
            .unwrap()
            .plan("Production")
            .unwrap();
        assert_eq!(
            plan.steps[0].preconditions,
            [
                Precondition::LeaseHeld(
                    "deploy:registry.example.invalid/ns/fullstack-fixture:Production".into()
                ),
                Precondition::WindowOpen("business-hours".into()),
                Precondition::HealthOk
            ]
        );
    }

    #[test]
    fn plan_revalidates_after_fields_are_mutated() {
        let mut template = DeployTemplate::parse(FIXTURE).unwrap();
        template.rollout.strategy = Strategy::Instant;
        assert!(matches!(
            template.plan("production"),
            Err(DeployError::InvalidField { .. })
        ));
    }

    fn arb_steps(min: usize) -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
        (min..=10usize)
            .prop_flat_map(|n| proptest::collection::btree_set(1u8..100, n - 1))
            .prop_map(|set| set.into_iter().chain(std::iter::once(100)).collect())
    }

    /// A `[risk]` section whose thresholds resolve to `class` for any score
    /// (the derived test below always plans with a fixed score), so the
    /// property test below can fuse the derived-resolution path into the
    /// same monotonicity check the static-class path already gets.
    ///
    /// `[risk.thresholds]` requires at least one field, and `highest_risk()`
    /// (used by `validate()`) is the highest class among the *present*
    /// fields regardless of their value -- so a derived template can never
    /// have `highest_risk() == Unused`. Callers must not use this for
    /// `RiskClass::Unused`; use a static `[risk]` section instead.
    fn derived_risk_section(class: RiskClass) -> String {
        debug_assert!(class >= RiskClass::Internal, "derived can't target Unused");
        let mut thresholds = vec!["internal = 0".to_string()];
        if class >= RiskClass::Edge {
            thresholds.push("edge = 1".to_string());
        }
        if class >= RiskClass::Core {
            thresholds.push("core = 2".to_string());
        }
        format!(
            "[risk]\nclass = \"derived\"\n[risk.thresholds]\n{}\n",
            thresholds.join("\n")
        )
    }

    const DERIVED_SCORE: u32 = 100;

    fn arb_template() -> impl proptest::strategy::Strategy<Value = (String, bool)> {
        (
            0..RiskClass::ALL.len(),
            0..Strategy::ALL.len(),
            any::<bool>(),
            any::<bool>(),
            1u8..=100,
            0i64..48,
        )
            .prop_map(|(c, s, production, derived, mirror, extra_hours)| {
                let class = RiskClass::ALL[c];
                // A derived template's highest_risk() is at least Internal
                // (see derived_risk_section), so it can never target Unused.
                let derived = derived && class >= RiskClass::Internal;
                (class, Strategy::ALL[s], production, derived, mirror, extra_hours)
            })
            .prop_filter("strategy allowed for class", |(c, s, ..)| strategy_allowed(*c, *s))
            .prop_flat_map(|(class, strategy, production, derived, mirror, extra_hours)| {
                let min = if matches!(strategy, Strategy::Instant | Strategy::BlueGreen) { 1 } else { min_steps(class) };
                let max = if matches!(strategy, Strategy::Instant | Strategy::BlueGreen) { 1 } else { 10 };
                arb_steps(min).prop_filter("step count", move |steps| steps.len() <= max).prop_map(move |steps| {
                    let n = i64::try_from(steps.len()).unwrap();
                    let hours = (min_span(class).num_hours() + n - 1) / n + extra_hours;
                    let envs = if production { "[\"sandbox\", \"production\"]" } else { "[\"sandbox\", \"staging\"]" };
                    let steps = steps.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
                    let shadow = if strategy == Strategy::ShadowThenGradual { format!("[shadow]\nenabled = true\nmirror_percent = {mirror}\ncompare = [\"status\"]\n") } else { String::new() };
                    let risk = if derived { derived_risk_section(class) } else { format!("[risk]\nclass = \"{class}\"\n") };
                    let src = format!("[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = {envs}\n{risk}[rollout]\nstrategy = \"{strategy}\"\nsteps = [{steps}]\nmin_step_duration = \"{hours}h\"\nwindows = \"business-hours\"\n{HEALTH}[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"1d\"\n{shadow}");
                    (src, derived)
                })
            })
    }

    proptest! {
        #[test]
        fn plan_traffic_is_monotonic_and_ends_at_100((src, derived) in arb_template()) {
            let template = DeployTemplate::parse(&src).unwrap();
            for env in &template.target.environments {
                let score = if derived { Some(DERIVED_SCORE) } else { None };
                let plan = template.plan_with_score(env, score).unwrap();
                let percents = percents(&plan);
                prop_assert!(percents.windows(2).all(|w| w[0] <= w[1]), "{percents:?}");
                prop_assert_eq!(percents.last().copied(), Some(100));
                let traffic: Vec<u8> = plan.steps.iter().filter(|s| s.kind == StepKind::Traffic).map(|s| s.traffic_percent).collect();
                prop_assert!(traffic.windows(2).all(|w| w[0] < w[1]), "{traffic:?}");
                prop_assert!(plan.steps.iter().enumerate().all(|(i, s)| s.index == i));
                prop_assert!(plan.steps.len() >= template.rollout.steps.len());
                prop_assert_eq!(plan.to_json()["steps"].as_array().unwrap().len(), plan.steps.len());
            }
        }
    }

    #[test]
    fn unlisted_environment_names_get_production_preconditions() {
        for env in ["prod", "live", "canary"] {
            let src = FIXTURE.replace("\"production\"", &format!("\"{env}\""));
            let plan = DeployTemplate::parse(&src).unwrap().plan(env).unwrap();
            for step in &plan.steps {
                assert!(
                    step.preconditions
                        .iter()
                        .any(|p| matches!(p, Precondition::WindowOpen(_))),
                    "{env}"
                );
            }
        }
    }
}
