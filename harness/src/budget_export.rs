use crate::budget::{BudgetClass, BudgetLimits, Usage};
use crate::telemetry::TelemetrySystem;

pub const SPENT: &str = "nanna_budget_spent";
pub const LIMIT: &str = "nanna_budget_limit";
pub const REMAINING: &str = "nanna_budget_remaining";

fn gauge(telemetry: &TelemetrySystem, name: &str, value: f64, labels: &[(&str, &str); 4]) {
    telemetry.record_gauge(name, value, labels.to_vec());
}

fn dimension(
    telemetry: &TelemetrySystem,
    labels: &[(&str, &str); 4],
    spent: f64,
    max: Option<f64>,
) {
    gauge(telemetry, SPENT, spent, labels);
    if let Some(limit) = max {
        gauge(telemetry, LIMIT, limit, labels);
        gauge(telemetry, REMAINING, (limit - spent).max(0.0), labels);
    }
}

/// ```
/// use harness::budget::{BudgetClass, BudgetLimits, Usage};
/// use harness::budget_export::BudgetObservation;
/// use harness::telemetry::TelemetrySystem;
///
/// let limits = BudgetLimits { max_count_per_task: Some(3), ..BudgetLimits::UNLIMITED };
/// let observation = BudgetObservation {
///     identity: "deployer".to_string(),
///     class: BudgetClass::Ci,
///     task: Usage { count: 1, minutes: 0.0 },
///     day: Usage { count: 4, minutes: 0.0 },
///     limits,
/// };
/// let telemetry = TelemetrySystem::new();
/// observation.record(&telemetry);
/// assert_eq!(telemetry.get_buffered_metrics_count(), 6);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetObservation {
    pub identity: String,
    pub class: BudgetClass,
    pub task: Usage,
    pub day: Usage,
    pub limits: BudgetLimits,
}

impl BudgetObservation {
    pub fn record(&self, telemetry: &TelemetrySystem) {
        let identity = crate::action_auditor::plain(&self.identity);
        let class = self.class.as_str();
        let limits = &self.limits;
        let task = (
            self.task,
            limits.max_count_per_task,
            limits.max_minutes_per_task,
        );
        let day = (
            self.day,
            limits.max_count_per_day,
            limits.max_minutes_per_day,
        );
        for (scope, (usage, count_max, minutes_max)) in [("task", task), ("day", day)] {
            let labels = |kind| {
                [
                    ("identity", identity.as_str()),
                    ("class", class),
                    ("scope", scope),
                    ("kind", kind),
                ]
            };
            dimension(
                telemetry,
                &labels("count"),
                f64::from(usage.count),
                count_max.map(f64::from),
            );
            dimension(telemetry, &labels("minutes"), usage.minutes, minutes_max);
        }
    }
}
