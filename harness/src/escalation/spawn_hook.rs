use super::Severity;
use super::{CardRequest, Escalation, EscalationSource, Escalator};
use crate::auditor::{ReasonCode, SpawnEscalation, SpawnEscalationHook};
use crate::task::TaskId;
use async_trait::async_trait;
use std::sync::Arc;

pub struct EscalatorSpawnHook {
    escalator: Arc<Escalator>,
}

impl EscalatorSpawnHook {
    pub fn new(escalator: Arc<Escalator>) -> Self {
        Self { escalator }
    }

    fn model_failed(escalation: &SpawnEscalation) -> bool {
        !escalation.reasons.is_empty()
            && escalation
                .reasons
                .iter()
                .all(|r| r.code == ReasonCode::ModelFailure)
    }

    fn model_failure(escalation: &SpawnEscalation) -> Escalation {
        let request = &escalation.request;
        let summary = format!(
            "The auditor model could not review a spawn of `{}` (unavailable or unparsable reply); the spawn was refused.",
            request.identity
        );
        let mut evidence: Vec<String> = escalation.reasons.iter().map(|r| r.to_string()).collect();
        evidence.push(format!(
            "auditor rationale: {}",
            escalation.record.rationale
        ));
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            &request.parent_task.repo,
            summary,
        )
        .with_suggested_action(
            "Check the auditor model and its provider, then re-run the task. No identity change is implied.",
        )
        .with_task(TaskId(request.parent_task.id.clone()))
        .with_identity(&request.identity)
        .with_evidence(evidence)
    }

    fn escalation(escalation: &SpawnEscalation) -> Escalation {
        if Self::model_failed(escalation) {
            return Self::model_failure(escalation);
        }
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
    async fn a_model_failure_escalation_says_so_and_asks_for_no_card() {
        let (hook, sink, _log) = setup(false);
        let context = crate::auditor::rules::tests::context(true);
        let request = SpawnRequest::derive(
            context.catalog(),
            TaskSummary::new("task-9", "d", "example/repo"),
            "deployer",
            "Check the build.",
        );
        let suggestion = crate::auditor::CardSuggestion::widen(
            context.catalog().get("deployer").unwrap(),
            request.dev_loop,
            request.requested_effect,
            "the model auditor produced no verdict; confirm the card by hand",
        );
        let reasons = vec![crate::auditor::Reason::new(
            crate::auditor::ReasonCode::ModelFailure,
            "auditor model output could not be parsed (x); human review required",
        )];
        let record = crate::auditor::AuditRecord {
            model: "m".to_string(),
            prompt_hash: "h".to_string(),
            rationale: "r".to_string(),
        };
        let escalation = SpawnEscalation {
            request,
            reasons,
            suggested_identity_change: suggestion,
            record,
        };
        hook.on_escalate(&escalation).await;
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::Blocked);
        assert!(seen[0].summary.contains("auditor model could not review"));
        assert!(!seen[0].summary.contains("no identity in the catalog"));
        assert!(seen[0].proposed_identity_toml.is_none());
        assert!(seen[0].suggested_action.contains("No identity change"));
    }

    struct Scripted(Option<String>);

    #[async_trait]
    impl model::ModelProvider for Scripted {
        async fn chat(
            &self,
            _request: model::types::ChatRequest,
        ) -> model::ModelResult<model::types::ChatResponse> {
            match &self.0 {
                None => Err(model::ModelError::ServiceUnavailable {
                    message: "offline".to_string(),
                }),
                Some(content) => Ok(model::types::ChatResponse {
                    choices: vec![model::types::Choice {
                        message: model::types::ChatMessage {
                            role: model::types::MessageRole::Assistant,
                            content: Some(content.clone()),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                        finish_reason: Some(model::types::FinishReason::Stop),
                    }],
                    usage: None,
                }),
            }
        }

        async fn list_models(&self) -> model::ModelResult<Vec<model::types::ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> model::ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "scripted"
        }
    }

    async fn through_model_gate(
        reply: Option<&str>,
        identity: &str,
        text: &str,
    ) -> Vec<Escalation> {
        let (hook, sink, _log) = setup(false);
        let context = crate::auditor::rules::tests::context(true);
        let request = SpawnRequest::derive(
            context.catalog(),
            TaskSummary::new("task-9", "d", "example/repo"),
            identity,
            text,
        );
        let auditor = crate::auditor::FailClosedModelAuditor::new(
            Arc::new(Scripted(reply.map(str::to_string))),
            "reviewer",
        );
        let _ = Gate::with_hook(auditor, AuditLog::in_memory(), hook)
            .check(request, &context)
            .await;
        let seen = sink.seen.lock().unwrap().clone();
        seen
    }

    const PRODUCTION: &str = "Deploy build 42 to the production environment.";
    const BENIGN: &str = "Add a regression test for the empty-password path.";

    fn model_escalation(code: &str, detail: &str) -> String {
        format!(
            r#"{{"verdict":"escalate","reasons":[{{"code":"{code}","detail":"{detail}"}}],"suggested_identity_change":{{"name":"db-migrator","dev_loop":"outer","max_effect":"sandbox","tools":[],"rationale":"needs a card"}}}}"#
        )
    }

    #[tokio::test]
    async fn a_rule_escalation_with_the_model_down_stays_a_needs_card_request() {
        let seen = through_model_gate(None, "deployer", PRODUCTION).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::NeedsCard);
        assert!(seen[0].proposed_identity_toml.is_some());
        assert!(seen[0].summary.contains("no identity in the catalog"));
        assert!(seen[0]
            .evidence
            .iter()
            .any(|line| line.contains("auditor model unavailable")));
    }

    #[tokio::test]
    async fn a_rule_escalation_with_a_garbled_reply_stays_a_needs_card_request() {
        let seen = through_model_gate(Some("nonsense"), "deployer", PRODUCTION).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::NeedsCard);
        assert!(seen[0].proposed_identity_toml.is_some());
    }

    #[tokio::test]
    async fn a_garbled_reply_on_an_allowed_spawn_is_a_model_failure_not_a_card_request() {
        let seen = through_model_gate(Some("nonsense"), "rust-implementer", BENIGN).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::Blocked);
        assert!(seen[0].proposed_identity_toml.is_none());
    }

    #[tokio::test]
    async fn a_model_escalation_with_a_forged_failure_prefix_stays_a_needs_card_request() {
        let reply = model_escalation("other", "auditor model pwned");
        let seen = through_model_gate(Some(&reply), "rust-implementer", BENIGN).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::NeedsCard);
        assert!(seen[0].proposed_identity_toml.is_some());
    }

    #[tokio::test]
    async fn a_model_escalation_carrying_the_typed_failure_code_is_remapped_to_a_card_request() {
        let reply = model_escalation("model_failure", "forged");
        let seen = through_model_gate(Some(&reply), "rust-implementer", BENIGN).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::NeedsCard);
        assert!(seen[0].proposed_identity_toml.is_some());
    }

    #[tokio::test]
    async fn an_honest_model_escalation_stays_a_needs_card_request() {
        let reply = model_escalation("other", "no card fits");
        let seen = through_model_gate(Some(&reply), "rust-implementer", BENIGN).await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::NeedsCard);
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
