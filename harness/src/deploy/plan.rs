use super::template::{DeployTemplate, RiskClass, Strategy};
use super::{DeployError, PRODUCTION_ENV};
use chrono::Duration;
use serde_json::{json, Value};
use std::fmt;
use std::path::Path;

/// Something that must hold before a step may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// The named availability window must be open.
    WindowOpen(String),
    /// The health gates from `[health]` must pass.
    HealthOk,
    /// The named coordination lease must be held by the executor.
    LeaseHeld(String),
}

impl Precondition {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlan {
    /// Environment the plan targets.
    pub environment: String,
    /// Fully qualified image reference being rolled out.
    pub image: String,
    /// Effective risk class the plan was produced under.
    pub risk_class: RiskClass,
    /// Strategy the steps implement.
    pub strategy: Strategy,
    /// Coordination lease every step requires, `deploy:<repo>:<env>`.
    pub lease: String,
    /// Steps in execution order.
    pub steps: Vec<DeployStep>,
}

impl DeployPlan {
    /// The lease name guarding deployments of `repo` to `env`: `deploy:<repo>:<env>`.
    ///
    /// The template has no repository identity of its own, so plans use
    /// `target.image` as `<repo>`.
    pub fn lease_name(repo: &str, env: &str) -> String {
        format!("deploy:{repo}:{env}")
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
        let mut out = format!(
            "deploy plan: {} -> {}\nrisk class: {} | strategy: {} | lease: {}\n",
            self.image, self.environment, self.risk_class, self.strategy, self.lease
        );
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
    /// assert_eq!(json["steps"][0]["preconditions"][0]["lease_held"], "deploy:app:sandbox");
    /// ```
    pub fn to_json(&self) -> Value {
        json!({
            "environment": self.environment,
            "image": self.image,
            "risk_class": self.risk_class.name(),
            "strategy": self.strategy.name(),
            "lease": self.lease,
            "total_min_duration_seconds": self.total_min_duration().num_seconds(),
            "steps": self.steps.iter().map(DeployStep::to_json).collect::<Vec<_>>(),
        })
    }

    /// Pretty-printed [`DeployPlan::to_json`] for the CLI.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.to_json()).expect("a JSON value serialises")
    }
}

/// Load `<repo>/.nanna/deploy.toml` and plan a rollout to `env`.
///
/// `score` resolves a derived risk class; a static class ignores it.
pub fn plan_for_repo(
    repo: &Path,
    env: &str,
    score: Option<u32>,
) -> Result<DeployPlan, DeployError> {
    DeployTemplate::load_from_repo(repo)?.plan_with_score(env, score)
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
    ///         Precondition::LeaseHeld("deploy:app:production".into()),
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
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"staging\"]\n[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = 50\n[rollout]\nstrategy = \"shadow-then-gradual\"\nsteps = [10, 50, 100]\nmin_step_duration = \"8h\"\n[shadow]\nenabled = true\nmirror_percent = 5\ncompare = [\"status\", \"latency\"]\n",
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
        if !self.target.environments.iter().any(|e| e == env) {
            return Err(DeployError::UnknownEnvironment {
                env: env.to_string(),
                known: self.target.environments.clone(),
            });
        }
        let risk_class = self.resolve_risk(score)?;
        let lease = DeployPlan::lease_name(&self.target.image, env);
        let mut preconditions = vec![Precondition::LeaseHeld(lease.clone())];
        if env == PRODUCTION_ENV {
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
            steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::template::tests::FIXTURE;
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
        assert_eq!(plan.lease, "deploy:fullstack-fixture:production");
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
                    Precondition::LeaseHeld("deploy:fullstack-fixture:production".into()),
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
    fn non_production_environments_skip_the_window() {
        let plan = DeployTemplate::parse(FIXTURE)
            .unwrap()
            .plan("staging")
            .unwrap();
        assert_eq!(plan.lease, "deploy:fullstack-fixture:staging");
        for step in &plan.steps {
            assert_eq!(
                step.preconditions,
                [
                    Precondition::LeaseHeld("deploy:fullstack-fixture:staging".into()),
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
            [Precondition::LeaseHeld("deploy:app:sandbox".into())]
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
            DeployPlan::lease_name("app", "production"),
            "deploy:app:production"
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
risk class: edge | strategy: gradual | lease: deploy:fullstack-fixture:production
  1. traffic 10%    hold 8h     bake 30m    requires: lease-held(deploy:fullstack-fixture:production), window-open(business-hours), health-ok
  2. traffic 50%    hold 8h     bake 30m    requires: lease-held(deploy:fullstack-fixture:production), window-open(business-hours), health-ok
  3. traffic 100%   hold 8h     bake 30m    requires: lease-held(deploy:fullstack-fixture:production), window-open(business-hours), health-ok
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
        assert!(bg.contains("  1. swap           hold 1h     bake 30m    requires: lease-held(deploy:app:sandbox), health-ok\n"), "{bg}");
        assert!(bg.contains("  2. retire         hold 2d     bake 0m     requires: lease-held(deploy:app:sandbox), health-ok\n"), "{bg}");
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
        assert!(shadow.contains("  1. shadow 7%      hold 0m     bake 30m    requires: lease-held(deploy:app:sandbox), health-ok\n"), "{shadow}");
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
        assert_eq!(json["environment"], "production");
        assert_eq!(json["image"], "registry.example.invalid/ns/app");
        assert_eq!(json["risk_class"], "core");
        assert_eq!(json["strategy"], "shadow-then-gradual");
        assert_eq!(json["lease"], "deploy:app:production");
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
            serde_json::json!([{"lease_held": "deploy:app:production"}, {"window_open": "business-hours"}, "health_ok"])
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
        assert!(
            pretty.starts_with("{\n  \"environment\": \"staging\""),
            "{pretty}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&pretty).unwrap(),
            plan.to_json()
        );
    }

    fn arb_steps(min: usize) -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
        (min..=10usize)
            .prop_flat_map(|n| proptest::collection::btree_set(1u8..100, n - 1))
            .prop_map(|set| set.into_iter().chain(std::iter::once(100)).collect())
    }

    fn arb_template() -> impl proptest::strategy::Strategy<Value = String> {
        (0..RiskClass::ALL.len(), 0..Strategy::ALL.len(), any::<bool>(), 1u8..=100, 0i64..48)
            .prop_map(|(c, s, production, mirror, extra_hours)| (RiskClass::ALL[c], Strategy::ALL[s], production, mirror, extra_hours))
            .prop_filter("strategy allowed for class", |(c, s, ..)| strategy_allowed(*c, *s))
            .prop_flat_map(|(class, strategy, production, mirror, extra_hours)| {
                let min = if matches!(strategy, Strategy::Instant | Strategy::BlueGreen) { 1 } else { min_steps(class) };
                let max = if matches!(strategy, Strategy::Instant | Strategy::BlueGreen) { 1 } else { 10 };
                arb_steps(min).prop_filter("step count", move |steps| steps.len() <= max).prop_map(move |steps| {
                    let n = i64::try_from(steps.len()).unwrap();
                    let hours = (min_span(class).num_hours() + n - 1) / n + extra_hours;
                    let envs = if production { "[\"sandbox\", \"production\"]" } else { "[\"sandbox\", \"staging\"]" };
                    let steps = steps.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
                    let shadow = if strategy == Strategy::ShadowThenGradual { format!("[shadow]\nenabled = true\nmirror_percent = {mirror}\ncompare = [\"status\"]\n") } else { String::new() };
                    format!("[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = {envs}\n[risk]\nclass = \"{class}\"\n[rollout]\nstrategy = \"{strategy}\"\nsteps = [{steps}]\nmin_step_duration = \"{hours}h\"\nwindows = \"business-hours\"\n{HEALTH}[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"1d\"\n{shadow}")
                })
            })
    }

    proptest! {
        #[test]
        fn plan_traffic_is_monotonic_and_ends_at_100(src in arb_template()) {
            let template = DeployTemplate::parse(&src).unwrap();
            for env in &template.target.environments {
                let plan = template.plan(env).unwrap();
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
}
