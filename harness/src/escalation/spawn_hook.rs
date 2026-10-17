use super::{CardRequest, Escalation, EscalationSource, Escalator};
use crate::auditor::{SpawnEscalation, SpawnEscalationHook};
use crate::task::TaskId;
use async_trait::async_trait;
use std::sync::Arc;

/// Records every auditor `Escalate` verdict through the [`Escalator`] as a
/// `needs-card` escalation, so it lands in the durable
/// [`EscalationLog`](super::EscalationLog) and in every configured sink.
///
/// The subtask text is deliberately left out of the escalation: it may be
/// attacker-controlled (a GitHub issue body) and escalations are published
/// to humans and external sinks.
pub struct EscalatorSpawnHook {
    escalator: Arc<Escalator>,
}

impl EscalatorSpawnHook {
    /// Deliver spawn escalations through `escalator`.
    pub fn new(escalator: Arc<Escalator>) -> Self {
        Self { escalator }
    }

    fn escalation(escalation: &SpawnEscalation) -> Escalation {
        let request = &escalation.request;
        let suggestion = &escalation.suggested_identity_change;
        let summary = format!(
            "The auditor escalated a spawn of `{}`: no identity in the catalog reaches `{}`.",
            request.identity, suggestion.max_effect
        );
        let card = CardRequest {
            name: suggestion.name.clone(),
            dev_loop: suggestion.dev_loop.to_string(),
            max_effect: suggestion.max_effect.to_string(),
            tools: suggestion.tools.clone(),
            reason: suggestion.rationale.clone(),
        };
        let mut evidence: Vec<String> = escalation.reasons.iter().map(|r| r.to_string()).collect();
        evidence.push(format!(
            "auditor rationale: {}",
            escalation.record.rationale
        ));
        Escalation::needs_card(
            EscalationSource::Auditor,
            &request.parent_task.repo,
            summary,
            &card,
        )
        .with_task(TaskId(request.parent_task.id.clone()))
        .with_identity(&request.identity)
        .with_evidence(evidence)
    }
}

#[async_trait]
impl SpawnEscalationHook for EscalatorSpawnHook {
    async fn on_escalate(&self, escalation: &SpawnEscalation) {
        if let Err(error) = self.escalator.escalate(Self::escalation(escalation)).await {
            tracing::error!(%error, "spawn escalation could not be delivered; the spawn stays refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::{AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary};
    use crate::escalation::sink::tests::RecordingSink;
    use crate::escalation::{EscalationLog, Severity};
    use crate::leases::SimulatedClock;
    use chrono::{Duration, TimeZone, Utc};

    fn setup(fail: bool) -> (EscalatorSpawnHook, Arc<RecordingSink>, Arc<EscalationLog>) {
        let sink = Arc::new(RecordingSink::new("rec", fail));
        let log = Arc::new(EscalationLog::in_memory());
        let clock = SimulatedClock::new(Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap());
        let escalator = Arc::new(Escalator::new(
            Arc::clone(&log),
            sink.clone(),
            Arc::new(clock),
            Duration::minutes(30),
        ));
        (EscalatorSpawnHook::new(escalator), sink, log)
    }

    async fn escalating_gate(hook: EscalatorSpawnHook) -> Result<(), crate::auditor::Refused> {
        let context = crate::auditor::rules::tests::context(true);
        let request = SpawnRequest::derive(
            context.catalog(),
            TaskSummary::new("task-9", "d", "example/repo"),
            "deployer",
            "Deploy build 42 to the production environment. SECRET-PAYLOAD",
        );
        Gate::with_hook(RuleAuditor::new(), AuditLog::in_memory(), hook)
            .check(request, &context)
            .await
            .map(|_| ())
    }

    #[tokio::test]
    async fn an_escalate_verdict_becomes_a_durable_needs_card_escalation() {
        let (hook, sink, log) = setup(false);
        let refused = escalating_gate(hook).await.unwrap_err();
        assert!(matches!(refused, crate::auditor::Refused::Verdict { .. }));
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let escalation = &seen[0];
        assert_eq!(escalation.severity, Severity::NeedsCard);
        assert_eq!(escalation.source, EscalationSource::Auditor);
        assert_eq!(escalation.identity.as_deref(), Some("deployer"));
        assert_eq!(escalation.task_id, Some(TaskId("task-9".to_string())));
        assert_eq!(escalation.repo, "example/repo");
        assert!(escalation.proposed_identity_toml.is_some());
        assert!(!escalation.evidence.is_empty());
        assert!(!escalation.summary.contains("SECRET-PAYLOAD"));
        assert!(escalation
            .evidence
            .iter()
            .all(|line| !line.contains("SECRET-PAYLOAD")));
        assert_eq!(log.states().len(), 1);
    }

    #[tokio::test]
    async fn a_failing_sink_still_records_the_escalation_and_the_spawn_stays_refused() {
        let (hook, _sink, log) = setup(true);
        let refused = escalating_gate(hook).await.unwrap_err();
        assert!(matches!(refused, crate::auditor::Refused::Verdict { .. }));
        assert_eq!(log.states().len(), 1);
        assert!(log.states()[0].last_delivered.is_none());
    }
}
