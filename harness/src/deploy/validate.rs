use super::template::{DeployTemplate, Health, RiskClass, Strategy};
use super::{DeployError, PRODUCTION_ENV};
use crate::windows::WindowSet;
use chrono::Duration;
use std::path::Path;

/// Whether a risk class permits a rollout strategy at all.
///
/// The class is a floor on caution, so `unused` allows everything while
/// `edge` and `core` insist on a stepped strategy.
///
/// ```
/// use harness::deploy::{strategy_allowed, RiskClass, Strategy};
///
/// assert!(strategy_allowed(RiskClass::Unused, Strategy::Instant));
/// assert!(!strategy_allowed(RiskClass::Internal, Strategy::Instant));
/// assert!(strategy_allowed(RiskClass::Internal, Strategy::BlueGreen));
/// assert!(!strategy_allowed(RiskClass::Core, Strategy::BlueGreen));
/// assert!(strategy_allowed(RiskClass::Core, Strategy::ShadowThenGradual));
/// ```
pub fn strategy_allowed(class: RiskClass, strategy: Strategy) -> bool {
    match (class, strategy) {
        (RiskClass::Unused, _) => true,
        (RiskClass::Internal, Strategy::Instant) => false,
        (RiskClass::Internal, _) => true,
        (RiskClass::Edge | RiskClass::Core, Strategy::Gradual | Strategy::ShadowThenGradual) => {
            true
        }
        (RiskClass::Edge | RiskClass::Core, Strategy::Instant | Strategy::BlueGreen) => false,
    }
}

/// Minimum number of traffic steps a class requires.
pub const fn min_steps(class: RiskClass) -> usize {
    match class {
        RiskClass::Unused | RiskClass::Internal => 1,
        RiskClass::Edge => 3,
        RiskClass::Core => 7,
    }
}

/// Minimum rollout span (`steps × min_step_duration`) a class requires.
pub fn min_span(class: RiskClass) -> Duration {
    match class {
        RiskClass::Unused | RiskClass::Internal => Duration::zero(),
        RiskClass::Edge => Duration::days(1),
        RiskClass::Core => Duration::days(7),
    }
}

fn describe(duration: Duration) -> String {
    let hours = duration.num_hours();
    if hours % 24 == 0 {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

fn invalid(file: &Path, field: &'static str, reason: String) -> DeployError {
    DeployError::InvalidField {
        file: file.to_path_buf(),
        field,
        reason,
    }
}

fn lacks_observation_window(health: Option<&Health>) -> bool {
    health.is_none_or(|h| h.bake_time <= Duration::zero())
}

impl DeployTemplate {
    pub(super) fn validate(&self) -> Result<(), DeployError> {
        let file = self.file();
        let class = self.highest_risk();
        let strategy = self.rollout.strategy;
        if !strategy_allowed(class, strategy) {
            let allowed = Strategy::ALL
                .into_iter()
                .filter(|s| strategy_allowed(class, *s))
                .map(Strategy::name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid(
                file,
                "rollout.strategy",
                format!("risk class `{class}` allows only: {allowed}"),
            ));
        }
        let steps = self.rollout.steps.len();
        if matches!(strategy, Strategy::Instant | Strategy::BlueGreen) && steps != 1 {
            return Err(invalid(
                file,
                "rollout.steps",
                format!("strategy `{strategy}` uses a single step of 100"),
            ));
        }
        if steps < min_steps(class) {
            return Err(invalid(
                file,
                "rollout.steps",
                format!(
                    "risk class `{class}` requires at least {} steps, got {steps}",
                    min_steps(class)
                ),
            ));
        }
        let span = self.rollout.span();
        if span < min_span(class) {
            return Err(invalid(file, "rollout.min_step_duration", format!("risk class `{class}` requires a rollout span of at least {}, got {} ({steps} steps x {})", describe(min_span(class)), describe(span), describe(self.rollout.min_step_duration))));
        }
        if self.target.environments.iter().any(|e| e == PRODUCTION_ENV) {
            if self.rollout.windows.is_none() {
                return Err(invalid(
                    file,
                    "rollout.windows",
                    format!("required when `{PRODUCTION_ENV}` is an environment"),
                ));
            }
            if self.health.is_none() {
                return Err(invalid(
                    file,
                    "health",
                    format!("section required when `{PRODUCTION_ENV}` is an environment"),
                ));
            }
        }
        let shadow_enabled = self.shadow.as_ref().is_some_and(|s| s.enabled);
        if strategy == Strategy::ShadowThenGradual && !shadow_enabled {
            return Err(invalid(
                file,
                "shadow.enabled",
                format!(
                    "must be true when rollout.strategy = {}",
                    Strategy::ShadowThenGradual
                ),
            ));
        }
        if shadow_enabled && strategy != Strategy::ShadowThenGradual {
            return Err(invalid(
                file,
                "shadow.enabled",
                format!(
                    "requires rollout.strategy = {}, got {strategy}",
                    Strategy::ShadowThenGradual
                ),
            ));
        }
        if shadow_enabled && lacks_observation_window(self.health.as_ref()) {
            return Err(invalid(
                file,
                "health",
                "section with a positive bake_time is required when shadow.enabled is true: it is the Shadow step's observation window".to_string(),
            ));
        }
        if strategy == Strategy::BlueGreen && self.rollback.retain_for <= Duration::zero() {
            return Err(invalid(file, "rollback.retain_for", format!("must be positive for strategy `{}`: the previous slot is retained before retirement", Strategy::BlueGreen)));
        }
        Ok(())
    }

    /// Check that the template's `rollout.windows` names a window in `windows`.
    ///
    /// ```
    /// use harness::deploy::{DeployError, DeployTemplate};
    /// use harness::windows::WindowSet;
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"production\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [100]\nwindows = \"business-hours\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n",
    /// )
    /// .unwrap();
    /// let windows = WindowSet::parse(
    ///     "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n",
    /// )
    /// .unwrap();
    /// assert!(template.validate_against(&windows).is_ok());
    /// let err = template.validate_against(&WindowSet::default()).unwrap_err();
    /// assert!(matches!(err, DeployError::InvalidField { field: "rollout.windows", .. }));
    /// ```
    pub fn validate_against(&self, windows: &WindowSet) -> Result<(), DeployError> {
        let Some(name) = self.rollout.windows.as_deref() else {
            return Ok(());
        };
        if windows.window(name).is_some() {
            return Ok(());
        }
        let known = windows
            .windows()
            .map(|w| w.name())
            .collect::<Vec<_>>()
            .join(", ");
        Err(invalid(
            self.file(),
            "rollout.windows",
            format!("unknown window `{name}` (known: {known})"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::template::tests::FIXTURE;
    use super::*;

    const HEAD: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\", \"production\"]\n";
    const TAIL: &str = "[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"1d\"\n";

    fn template(class: RiskClass, strategy: Strategy, steps: &str, min_step: &str) -> String {
        let shadow_enabled = strategy == Strategy::ShadowThenGradual;
        format!(
            "{HEAD}[risk]\nclass = \"{class}\"\n[rollout]\nstrategy = \"{strategy}\"\nsteps = {steps}\nmin_step_duration = \"{min_step}\"\nwindows = \"business-hours\"\n{TAIL}[shadow]\nenabled = {shadow_enabled}\nmirror_percent = 10\ncompare = [\"status\"]\n"
        )
    }

    fn generous(class: RiskClass, strategy: Strategy) -> String {
        match strategy {
            Strategy::Instant | Strategy::BlueGreen => template(class, strategy, "[100]", "1h"),
            Strategy::Gradual | Strategy::ShadowThenGradual => {
                template(class, strategy, "[1, 5, 10, 25, 50, 75, 100]", "1d")
            }
        }
    }

    fn field_error(src: &str) -> (&'static str, String) {
        match DeployTemplate::parse(src).unwrap_err() {
            DeployError::InvalidField { field, reason, .. } => (field, reason),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn matrix_is_exhaustive() {
        let expected = [
            (RiskClass::Unused, [true, true, true, true]),
            (RiskClass::Internal, [false, true, true, true]),
            (RiskClass::Edge, [false, false, true, true]),
            (RiskClass::Core, [false, false, true, true]),
        ];
        for (class, allowed) in expected {
            for (strategy, allowed) in Strategy::ALL.into_iter().zip(allowed) {
                assert_eq!(
                    strategy_allowed(class, strategy),
                    allowed,
                    "{class} x {strategy}"
                );
                let result = DeployTemplate::parse(&generous(class, strategy));
                assert_eq!(result.is_ok(), allowed, "{class} x {strategy}: {result:?}");
                if !allowed {
                    let (field, reason) = field_error(&generous(class, strategy));
                    assert_eq!(field, "rollout.strategy");
                    assert!(reason.contains(class.name()), "{reason}");
                }
            }
        }
    }

    #[test]
    fn matrix_covers_every_class_and_strategy() {
        let mut seen = 0;
        for class in RiskClass::ALL {
            for strategy in Strategy::ALL {
                let _ = strategy_allowed(class, strategy);
                seen += 1;
            }
        }
        assert_eq!(seen, RiskClass::ALL.len() * Strategy::ALL.len());
    }

    #[test]
    fn single_step_strategies_take_exactly_one_step() {
        for strategy in [Strategy::Instant, Strategy::BlueGreen] {
            let (field, reason) =
                field_error(&template(RiskClass::Unused, strategy, "[50, 100]", "1h"));
            assert_eq!(field, "rollout.steps");
            assert!(reason.contains(strategy.name()), "{reason}");
        }
    }

    #[test]
    fn internal_allows_a_single_gated_step() {
        assert!(DeployTemplate::parse(&template(
            RiskClass::Internal,
            Strategy::Gradual,
            "[100]",
            "0m"
        ))
        .is_ok());
        assert!(DeployTemplate::parse(&template(
            RiskClass::Internal,
            Strategy::Gradual,
            "[10, 100]",
            "0m"
        ))
        .is_ok());
    }

    #[test]
    fn edge_needs_three_steps_over_a_day() {
        assert!(DeployTemplate::parse(&template(
            RiskClass::Edge,
            Strategy::Gradual,
            "[10, 50, 100]",
            "8h"
        ))
        .is_ok());
        let (field, reason) = field_error(&template(
            RiskClass::Edge,
            Strategy::Gradual,
            "[50, 100]",
            "1d",
        ));
        assert_eq!(field, "rollout.steps");
        assert!(reason.contains("at least 3"), "{reason}");
        let (field, reason) = field_error(&template(
            RiskClass::Edge,
            Strategy::Gradual,
            "[10, 50, 100]",
            "7h",
        ));
        assert_eq!(field, "rollout.min_step_duration");
        assert!(reason.contains("1d") && reason.contains("21h"), "{reason}");
    }

    #[test]
    fn core_needs_seven_steps_over_a_week() {
        assert!(DeployTemplate::parse(&template(
            RiskClass::Core,
            Strategy::Gradual,
            "[1, 5, 10, 25, 50, 75, 100]",
            "1d"
        ))
        .is_ok());
        assert!(DeployTemplate::parse(&template(
            RiskClass::Core,
            Strategy::ShadowThenGradual,
            "[1, 5, 10, 25, 50, 75, 100]",
            "24h"
        ))
        .is_ok());
        let (field, reason) = field_error(&template(
            RiskClass::Core,
            Strategy::Gradual,
            "[1, 5, 10, 25, 50, 100]",
            "2d",
        ));
        assert_eq!(field, "rollout.steps");
        assert!(reason.contains("at least 7"), "{reason}");
        let (field, reason) = field_error(&template(
            RiskClass::Core,
            Strategy::Gradual,
            "[1, 5, 10, 25, 50, 75, 100]",
            "23h",
        ));
        assert_eq!(field, "rollout.min_step_duration");
        assert!(reason.contains("7d"), "{reason}");
    }

    #[test]
    fn derived_templates_validate_against_their_highest_class() {
        let src = template(RiskClass::Edge, Strategy::Gradual, "[10, 50, 100]", "8h").replace(
            "class = \"edge\"",
            "class = \"derived\"\n[risk.thresholds]\ninternal = 10\ncore = 100",
        );
        let (field, reason) = field_error(&src);
        assert_eq!(field, "rollout.steps");
        assert!(reason.contains("core"), "{reason}");
        let edge_only = src.replace("core = 100", "edge = 100");
        assert!(DeployTemplate::parse(&edge_only).is_ok());
    }

    #[test]
    fn production_requires_windows_and_health() {
        let (field, reason) = field_error(&FIXTURE.replace("windows = \"business-hours\"\n", ""));
        assert_eq!(field, "rollout.windows");
        assert!(reason.contains("production"), "{reason}");
        let no_health = FIXTURE.replace("[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n", "");
        let (field, reason) = field_error(&no_health);
        assert_eq!(field, "health");
        assert!(reason.contains("production"), "{reason}");
        let staging_only = no_health
            .replace(
                "environments = [\"sandbox\", \"staging\", \"production\"]",
                "environments = [\"sandbox\", \"staging\"]",
            )
            .replace("windows = \"business-hours\"\n", "");
        assert!(DeployTemplate::parse(&staging_only).is_ok());
    }

    #[test]
    fn shadow_and_strategy_must_agree() {
        let shadow_off = template(
            RiskClass::Unused,
            Strategy::ShadowThenGradual,
            "[100]",
            "0m",
        )
        .replace("enabled = true", "enabled = false");
        let (field, reason) = field_error(&shadow_off);
        assert_eq!(field, "shadow.enabled");
        assert!(reason.contains("shadow-then-gradual"), "{reason}");
        let no_shadow_section = template(
            RiskClass::Unused,
            Strategy::ShadowThenGradual,
            "[100]",
            "0m",
        )
        .replace(
            "[shadow]\nenabled = true\nmirror_percent = 10\ncompare = [\"status\"]\n",
            "",
        );
        assert_eq!(field_error(&no_shadow_section).0, "shadow.enabled");
        let shadow_on_gradual = template(RiskClass::Unused, Strategy::Gradual, "[100]", "0m")
            .replace("enabled = false", "enabled = true");
        let (field, reason) = field_error(&shadow_on_gradual);
        assert_eq!(field, "shadow.enabled");
        assert!(reason.contains("shadow-then-gradual"), "{reason}");
    }

    #[test]
    fn shadow_requires_a_positive_observation_window() {
        let base = template(
            RiskClass::Unused,
            Strategy::ShadowThenGradual,
            "[100]",
            "0m",
        );
        assert!(
            DeployTemplate::parse(&base).is_ok(),
            "the base template is valid"
        );
        let no_bake = base.replace("bake_time = \"30m\"", "bake_time = \"0m\"");
        let (field, reason) = field_error(&no_bake);
        assert_eq!(field, "health");
        assert!(reason.contains("Shadow step"), "{reason}");
    }

    #[test]
    fn blue_green_retains_the_previous_slot() {
        let src = template(RiskClass::Internal, Strategy::BlueGreen, "[100]", "1h")
            .replace("retain_for = \"1d\"\n", "");
        let (field, reason) = field_error(&src);
        assert_eq!(field, "rollback.retain_for");
        assert!(reason.contains("blue-green"), "{reason}");
    }

    const WINDOWS: &str = "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n[[window]]\nname = \"oncall\"\ntimezone = \"UTC\"\ndays = [\"tue\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n";

    #[test]
    fn validate_against_accepts_known_window() {
        let template = DeployTemplate::parse(FIXTURE).unwrap();
        template
            .validate_against(&WindowSet::parse(WINDOWS).unwrap())
            .unwrap();
    }

    #[test]
    fn validate_against_rejects_unknown_window() {
        let template =
            DeployTemplate::parse(&FIXTURE.replace("business-hours", "weekends")).unwrap();
        let err = template
            .validate_against(&WindowSet::parse(WINDOWS).unwrap())
            .unwrap_err();
        match err {
            DeployError::InvalidField { field, reason, .. } => {
                assert_eq!(field, "rollout.windows");
                assert!(
                    reason.contains("weekends") && reason.contains("business-hours, oncall"),
                    "{reason}"
                );
            }
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn validate_against_ignores_templates_without_windows() {
        let src = template(RiskClass::Unused, Strategy::Instant, "[100]", "0m")
            .replace("windows = \"business-hours\"\n", "")
            .replace(
                "environments = [\"sandbox\", \"production\"]",
                "environments = [\"sandbox\"]",
            );
        let template = DeployTemplate::parse(&src).unwrap();
        template.validate_against(&WindowSet::default()).unwrap();
    }
}
