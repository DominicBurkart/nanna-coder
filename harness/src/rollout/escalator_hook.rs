use super::hooks::{EscalationHook, RolloutEscalation};
use crate::deploy::is_production_env;
use crate::escalation::{Escalation, EscalationSource, Escalator, Severity};
use async_trait::async_trait;
use std::sync::Arc;

/// Delivers every rollout stop through the [`Escalator`]: a production
/// rollout raises an `incident` (setting the production hold in the shared
/// log), any other environment raises `blocked`. A stop the incident
/// responder worked on is attributed to the `incident` source, with its
/// postmortem as evidence.
pub struct EscalatorHook {
    escalator: Arc<Escalator>,
    repo: String,
}

impl EscalatorHook {
    /// Escalate against `repo` (`owner/name`), the repository the sinks
    /// file in and the production hold is keyed by.
    pub fn new(escalator: Arc<Escalator>, repo: impl Into<String>) -> Self {
        Self {
            escalator,
            repo: repo.into(),
        }
    }

    fn escalation(&self, stop: &RolloutEscalation) -> Escalation {
        let severity = if is_production_env(&stop.environment) {
            Severity::Incident
        } else {
            Severity::Blocked
        };
        let source = if stop.postmortem.is_some() {
            EscalationSource::Incident
        } else {
            EscalationSource::Rollout
        };
        let mut evidence = vec![format!(
            "rollout {} to {} ({} -> {}) at step {} with {}% traffic",
            stop.rollout_id,
            stop.environment,
            stop.previous_image,
            stop.image,
            stop.step,
            stop.traffic_percent
        )];
        if let Some(breach) = &stop.breach {
            evidence.push(format!("breach: {breach}"));
            evidence.extend(
                breach
                    .evidence
                    .iter()
                    .map(|sample| format!("{} answered {}", sample.endpoint, sample.status)),
            );
        }
        if let Some(postmortem) = &stop.postmortem {
            evidence.push(postmortem.body.clone());
        }
        Escalation::new(severity, source, &self.repo, &stop.summary)
            .with_evidence(evidence)
            .with_suggested_action(format!(
                "inspect with `nanna deploy status {id}`, then `nanna deploy roll-forward {id} --image <ref> --pr <url>` or resolve the incident hold with `nanna escalation resolve <escalation id>`",
                id = stop.rollout_id
            ))
    }
}

#[async_trait]
impl EscalationHook for EscalatorHook {
    async fn escalate(&self, escalation: &RolloutEscalation) -> Result<(), String> {
        self.escalator
            .escalate(self.escalation(escalation))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::{
        DeliveryOutcome, DeliveryReceipt, EscalationError, EscalationLog, EscalationSink,
    };
    use std::sync::Mutex;

    struct RecordingSink {
        fail: bool,
        seen: Mutex<Vec<Escalation>>,
    }

    impl RecordingSink {
        fn new(_name: &str, fail: bool) -> Self {
            Self {
                fail,
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl EscalationSink for RecordingSink {
        fn name(&self) -> &str {
            "rec"
        }

        async fn deliver(
            &self,
            escalation: &Escalation,
        ) -> Result<DeliveryReceipt, EscalationError> {
            self.seen.lock().unwrap().push(escalation.clone());
            if self.fail {
                return Err(EscalationError::UnknownHold("rec refused".into()));
            }
            Ok(DeliveryReceipt {
                sink: "rec".into(),
                reference: escalation.id.clone(),
                outcome: DeliveryOutcome::Posted,
            })
        }
    }

    use crate::leases::SimulatedClock;
    use crate::rollout::health::{
        EvidenceSample, HealthBreach, HealthObservation, HealthThreshold,
    };
    use crate::rollout::incident::Postmortem;
    use chrono::{Duration, TimeZone, Utc};

    fn hook(fail: bool) -> (EscalatorHook, Arc<RecordingSink>, Arc<EscalationLog>) {
        let log = Arc::new(EscalationLog::in_memory());
        let sink = Arc::new(RecordingSink::new("rec", fail));
        let clock = Arc::new(SimulatedClock::new(
            Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap(),
        ));
        let escalator = Arc::new(Escalator::new(
            log.clone(),
            sink.clone(),
            clock,
            Duration::minutes(30),
        ));
        (EscalatorHook::new(escalator, "example/repo"), sink, log)
    }

    fn stop(environment: &str) -> RolloutEscalation {
        RolloutEscalation {
            rollout_id: "rollout-1".into(),
            environment: environment.into(),
            image: "app:v2".into(),
            previous_image: "app:v1".into(),
            step: 2,
            traffic_percent: 50,
            summary: "health source unavailable".into(),
            breach: None,
            postmortem: None,
        }
    }

    fn breach() -> HealthBreach {
        HealthBreach {
            threshold: HealthThreshold::ErrorRateMax(0.05),
            observed: HealthObservation::ErrorRate(0.2),
            step: 2,
            evidence: vec![EvidenceSample {
                endpoint: "/health/v1".into(),
                status: 503,
            }],
        }
    }

    #[tokio::test]
    async fn a_production_stop_raises_an_incident_and_sets_the_hold() {
        let (hook, sink, log) = hook(false);
        let mut production = stop("production");
        production.breach = Some(breach());
        hook.escalate(&production).await.unwrap();
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::Incident);
        assert_eq!(seen[0].source, EscalationSource::Rollout);
        assert_eq!(seen[0].repo, "example/repo");
        assert!(seen[0]
            .evidence
            .iter()
            .any(|l| l.contains("/health/v1 answered 503")));
        assert!(seen[0].evidence.iter().any(|l| l.starts_with("breach:")));
        assert!(seen[0]
            .suggested_action
            .contains("nanna deploy status rollout-1"));
        assert!(log.production_held("example/repo"));
    }

    #[tokio::test]
    async fn a_non_production_stop_is_blocked_without_a_hold() {
        let (hook, sink, log) = hook(false);
        hook.escalate(&stop("staging")).await.unwrap();
        assert_eq!(sink.seen.lock().unwrap()[0].severity, Severity::Blocked);
        assert!(!log.production_held("example/repo"));
    }

    #[tokio::test]
    async fn an_incident_responder_stop_carries_its_postmortem_and_source() {
        let (hook, sink, _log) = hook(false);
        let mut worked = stop("production");
        worked.postmortem = Some(Postmortem {
            title: "t".into(),
            body: "postmortem body".into(),
        });
        hook.escalate(&worked).await.unwrap();
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen[0].source, EscalationSource::Incident);
        assert!(seen[0].evidence.contains(&"postmortem body".to_string()));
    }

    #[tokio::test]
    async fn a_failed_delivery_is_reported_but_the_hold_stays() {
        let (hook, _sink, log) = hook(true);
        let err = hook.escalate(&stop("production")).await.unwrap_err();
        assert!(err.contains("rec refused"));
        assert!(log.production_held("example/repo"));
    }

    #[tokio::test]
    async fn a_repeat_inside_the_window_collapses_into_the_counter() {
        let (hook, sink, _log) = hook(false);
        hook.escalate(&stop("production")).await.unwrap();
        hook.escalate(&stop("production")).await.unwrap();
        assert_eq!(sink.seen.lock().unwrap().len(), 1);
    }
}
