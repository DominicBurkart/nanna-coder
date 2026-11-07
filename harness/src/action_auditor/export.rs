use super::{ActionReview, ActionVerdict};
use crate::auditor::{ReasonCode, VerdictKind};
use crate::telemetry::TelemetrySystem;
use serde::Serialize;
use std::sync::Arc;

pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_REASON_CODES: usize = 8;

fn plain(text: &str) -> String {
    let keep = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    let reduced = text.chars().take(MAX_NAME_CHARS);
    reduced.map(|c| if keep(c) { c } else { '?' }).collect()
}

fn verdict_name(kind: VerdictKind) -> &'static str {
    match kind {
        VerdictKind::Allow => "allow",
        VerdictKind::Block => "block",
        VerdictKind::Escalate => "escalate",
    }
}

/// ```
/// use harness::action_auditor::{ActionAuditEvent, ActionReview, ActionVerdict};
/// use harness::auditor::{Reason, ReasonCode};
/// use harness::effects::EffectClass;
/// use harness::task::TaskId;
/// use harness::telemetry::TelemetrySystem;
///
/// let review = ActionReview {
///     identity: "deployer".to_string(),
///     task_id: TaskId("t1".to_string()),
///     tool: "sandbox_deploy".to_string(),
///     args: serde_json::json!({ "token": "sk-live-SECRET" }),
///     effect_class: EffectClass::Sandbox,
///     prior_actions: vec![],
/// };
/// let verdict = ActionVerdict::block(vec![Reason::new(ReasonCode::WindowClosed, "window closed: sk-live-SECRET")]);
/// let event = ActionAuditEvent::new(&review, &verdict, "rule-action-auditor");
/// assert_eq!(event.verdict, "block");
/// assert_eq!(event.reason_codes, [ReasonCode::WindowClosed]);
/// assert!(!serde_json::to_string(&event).unwrap().contains("SECRET"));
///
/// let telemetry = TelemetrySystem::new();
/// event.record(&telemetry);
/// assert_eq!(telemetry.get_buffered_metrics_count(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionAuditEvent {
    pub verdict: &'static str,
    pub reason_codes: Vec<ReasonCode>,
    pub identity: String,
    pub tool: String,
    pub task_id: String,
    pub effect_class: &'static str,
    pub auditor: String,
}

impl ActionAuditEvent {
    pub fn new(review: &ActionReview, verdict: &ActionVerdict, auditor: &str) -> Self {
        let codes = verdict.reasons().iter().map(|reason| reason.code);
        Self {
            verdict: verdict_name(verdict.kind()),
            reason_codes: codes.take(MAX_REASON_CODES).collect(),
            identity: plain(&review.identity),
            tool: plain(&review.tool),
            task_id: plain(&review.task_id.0),
            effect_class: review.effect_class.as_str(),
            auditor: plain(auditor),
        }
    }

    pub fn record(&self, telemetry: &TelemetrySystem) {
        let labels = vec![
            ("verdict", self.verdict),
            ("effect_class", self.effect_class),
            ("identity", self.identity.as_str()),
            ("tool", self.tool.as_str()),
        ];
        telemetry.record_counter("nanna_action_audit_total", 1.0, labels);
        let data = serde_json::to_value(self).unwrap_or_default();
        telemetry.record_event("action_audit", "action_audit", data);
    }
}

/// ```
/// use harness::action_auditor::{ActionAuditEvent, ActionAuditSink, NullSink};
///
/// fn takes_a_sink(_sink: &dyn ActionAuditSink) {}
/// takes_a_sink(&NullSink);
/// ```
pub trait ActionAuditSink: Send + Sync {
    fn emit(&self, event: ActionAuditEvent);
}

/// ```
/// use harness::action_auditor::{ActionAuditSink, NullSink};
///
/// let sink: &dyn ActionAuditSink = &NullSink;
/// # let _ = sink;
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl ActionAuditSink for NullSink {
    fn emit(&self, _event: ActionAuditEvent) {}
}

/// ```
/// use harness::action_auditor::TelemetrySink;
/// use harness::telemetry::TelemetrySystem;
/// use std::sync::Arc;
///
/// let telemetry = Arc::new(TelemetrySystem::new());
/// let sink = TelemetrySink::new(Arc::clone(&telemetry));
/// # let _ = sink;
/// assert_eq!(telemetry.get_buffered_metrics_count(), 0);
/// ```
pub struct TelemetrySink {
    telemetry: Arc<TelemetrySystem>,
}

impl TelemetrySink {
    pub fn new(telemetry: Arc<TelemetrySystem>) -> Self {
        Self { telemetry }
    }
}

impl ActionAuditSink for TelemetrySink {
    fn emit(&self, event: ActionAuditEvent) {
        event.record(&self.telemetry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::Reason;
    use crate::effects::EffectClass;
    use crate::task::TaskId;

    fn review(identity: &str, tool: &str) -> ActionReview {
        ActionReview {
            identity: identity.to_string(),
            task_id: TaskId("task 1".to_string()),
            tool: tool.to_string(),
            args: serde_json::json!({ "k": "sk-live-SECRET" }),
            effect_class: EffectClass::Repository,
            prior_actions: vec![EffectClass::Ci],
        }
    }

    #[test]
    fn names_are_reduced_to_plain_characters_and_clipped() {
        assert_eq!(plain("a b@c\nd"), "a?b?c?d");
        assert_eq!(plain(&"z".repeat(500)).chars().count(), MAX_NAME_CHARS);
        let allow = ActionVerdict::Allow;
        let event = ActionAuditEvent::new(&review("x y", "t.o-ol_1"), &allow, "m\u{7}");
        assert_eq!(
            (event.identity.as_str(), event.tool.as_str()),
            ("x?y", "t.o-ol_1")
        );
        assert_eq!(
            (event.task_id.as_str(), event.auditor.as_str()),
            ("task?1", "m?")
        );
    }

    #[test]
    fn at_most_eight_reason_codes_are_carried_in_order() {
        let code = |n: usize| {
            if n == 0 {
                ReasonCode::ScopeCreep
            } else {
                ReasonCode::Other
            }
        };
        let reasons: Vec<Reason> = (0..12).map(|n| Reason::new(code(n), "detail")).collect();
        let verdict = ActionVerdict::escalate(reasons);
        let event = ActionAuditEvent::new(&review("a", "t"), &verdict, "m");
        assert_eq!(event.reason_codes.len(), MAX_REASON_CODES);
        assert_eq!(event.reason_codes[0], ReasonCode::ScopeCreep);
        assert_eq!(event.verdict, "escalate");
        let allow = ActionAuditEvent::new(&review("a", "t"), &ActionVerdict::Allow, "m");
        assert!(allow.reason_codes.is_empty());
        assert_eq!(allow.verdict, "allow");
    }

    #[test]
    fn recording_buffers_one_labelled_counter() {
        let telemetry = TelemetrySystem::new();
        let verdict = ActionVerdict::block(vec![Reason::new(ReasonCode::WindowClosed, "closed")]);
        let event = ActionAuditEvent::new(&review("deployer", "sandbox_deploy"), &verdict, "rule");
        event.record(&telemetry);
        assert_eq!(telemetry.get_buffered_metrics_count(), 1);
    }

    #[test]
    fn the_sinks_accept_events_and_the_telemetry_sink_buffers_them() {
        let event = ActionAuditEvent::new(&review("a", "t"), &ActionVerdict::Allow, "m");
        NullSink.emit(event.clone());
        let telemetry = Arc::new(TelemetrySystem::new());
        TelemetrySink::new(Arc::clone(&telemetry)).emit(event);
        assert_eq!(telemetry.get_buffered_metrics_count(), 1);
        assert_eq!(verdict_name(VerdictKind::Escalate), "escalate");
    }
}
