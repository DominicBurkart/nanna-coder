use super::health::HealthBreach;
use super::state::RolloutRecord;
use crate::deploy::DeployStep;
use async_trait::async_trait;
use std::sync::Mutex;
use thiserror::Error;

/// The auditor refused to let a step run.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("audit denied step: {reason}")]
pub struct AuditDenied {
    /// Why the step was refused.
    pub reason: String,
}

/// Reviews each step before its traffic is applied. The action auditor
/// provides the real implementation; [`NoAudit`] approves everything.
#[async_trait]
pub trait AuditHook: Send + Sync {
    /// Approve `step` of `record`, or deny it with a reason.
    async fn review_step(
        &self,
        record: &RolloutRecord,
        step: &DeployStep,
    ) -> Result<(), AuditDenied>;
}

/// Approves every step.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAudit;

#[async_trait]
impl AuditHook for NoAudit {
    async fn review_step(
        &self,
        _record: &RolloutRecord,
        _step: &DeployStep,
    ) -> Result<(), AuditDenied> {
        Ok(())
    }
}

/// Records every review and can deny from a given step on.
#[derive(Debug, Default)]
pub struct RecordingAudit {
    reviews: Mutex<Vec<(String, usize)>>,
    deny_from: Mutex<Option<(usize, String)>>,
}

impl RecordingAudit {
    /// Deny every step with index at least `step`, giving `reason`.
    pub fn deny_from(&self, step: usize, reason: &str) {
        *self.deny_from.lock().unwrap() = Some((step, reason.to_string()));
    }

    /// Every `(rollout id, step index)` reviewed so far.
    pub fn reviews(&self) -> Vec<(String, usize)> {
        self.reviews.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuditHook for RecordingAudit {
    async fn review_step(
        &self,
        record: &RolloutRecord,
        step: &DeployStep,
    ) -> Result<(), AuditDenied> {
        self.reviews
            .lock()
            .unwrap()
            .push((record.id.clone(), step.index));
        match self.deny_from.lock().unwrap().as_ref() {
            Some((from, reason)) if step.index >= *from => Err(AuditDenied {
                reason: reason.clone(),
            }),
            _ => Ok(()),
        }
    }
}

/// What the executor hands to a human when it halts.
#[derive(Debug, Clone, PartialEq)]
pub struct RolloutEscalation {
    /// Rollout that halted.
    pub rollout_id: String,
    /// Environment being rolled out to.
    pub environment: String,
    /// Image being rolled out.
    pub image: String,
    /// Image that was live before the rollout.
    pub previous_image: String,
    /// Step the rollout halted at.
    pub step: usize,
    /// Traffic share the new image holds while halted.
    pub traffic_percent: u8,
    /// Why the rollout halted.
    pub summary: String,
    /// The breach, when a health gate caused the halt.
    pub breach: Option<HealthBreach>,
}

/// Receives halt-and-escalate events. The escalation paths work provides
/// the real sinks; [`LogEscalation`] only logs.
#[async_trait]
pub trait EscalationHook: Send + Sync {
    /// Hand `escalation` to a human. An error is reported by the executor
    /// after the rollout is already halted.
    async fn escalate(&self, escalation: &RolloutEscalation) -> Result<(), String>;
}

/// Logs the escalation at error level.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogEscalation;

#[async_trait]
impl EscalationHook for LogEscalation {
    async fn escalate(&self, escalation: &RolloutEscalation) -> Result<(), String> {
        tracing::error!(rollout = %escalation.rollout_id, step = escalation.step, traffic = escalation.traffic_percent, "Rollout halted: {}", escalation.summary);
        Ok(())
    }
}

/// Records every escalation and can be scripted to fail.
#[derive(Debug, Default)]
pub struct RecordingEscalation {
    escalations: Mutex<Vec<RolloutEscalation>>,
    failing: Mutex<bool>,
}

impl RecordingEscalation {
    /// Every escalation received so far.
    pub fn escalations(&self) -> Vec<RolloutEscalation> {
        self.escalations.lock().unwrap().clone()
    }

    /// Make every escalation fail (or succeed again).
    pub fn set_failing(&self, failing: bool) {
        *self.failing.lock().unwrap() = failing;
    }
}

#[async_trait]
impl EscalationHook for RecordingEscalation {
    async fn escalate(&self, escalation: &RolloutEscalation) -> Result<(), String> {
        self.escalations.lock().unwrap().push(escalation.clone());
        if *self.failing.lock().unwrap() {
            return Err("scripted failure".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollout::state::tests::record;

    #[tokio::test]
    async fn no_audit_approves_and_recording_audit_denies_from_a_step() {
        let r = record("production");
        assert!(NoAudit.review_step(&r, &r.plan.steps[2]).await.is_ok());
        let audit = RecordingAudit::default();
        assert!(audit.review_step(&r, &r.plan.steps[0]).await.is_ok());
        audit.deny_from(1, "blast radius too large");
        assert!(audit.review_step(&r, &r.plan.steps[0]).await.is_ok());
        let err = audit.review_step(&r, &r.plan.steps[1]).await.unwrap_err();
        assert_eq!(err.to_string(), "audit denied step: blast radius too large");
        assert_eq!(
            audit.reviews(),
            vec![
                ("rollout-1".to_string(), 0),
                ("rollout-1".to_string(), 0),
                ("rollout-1".to_string(), 1)
            ]
        );
    }

    fn escalation() -> RolloutEscalation {
        RolloutEscalation {
            rollout_id: "rollout-1".into(),
            environment: "production".into(),
            image: "app:v2".into(),
            previous_image: "app:v1".into(),
            step: 2,
            traffic_percent: 50,
            summary: "health breach".into(),
            breach: None,
        }
    }

    #[tokio::test]
    async fn log_escalation_succeeds_and_recording_escalation_can_fail() {
        assert!(LogEscalation.escalate(&escalation()).await.is_ok());
        let hook = RecordingEscalation::default();
        assert!(hook.escalate(&escalation()).await.is_ok());
        hook.set_failing(true);
        assert_eq!(
            hook.escalate(&escalation()).await.unwrap_err(),
            "scripted failure"
        );
        assert_eq!(hook.escalations().len(), 2);
        assert_eq!(hook.escalations()[0], escalation());
    }
}
