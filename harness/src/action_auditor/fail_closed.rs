//! A timeout wrapper that keeps a slow or broken action auditor fail-closed.

use super::{ActionAuditError, ActionAuditor, ActionContext, ActionReview, ActionVerdict};
use async_trait::async_trait;
use std::time::Duration;

/// Default time an inner auditor gets before the review fails closed.
pub const ACTION_AUDIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Wraps an [`ActionAuditor`] so that no review can hang or silently pass.
///
/// An inner error is returned unchanged and a review that outlives the
/// timeout becomes [`ActionAuditError::Timeout`]. [`ActionGate`](super::ActionGate)
/// maps every `Err` to a `Block`, so neither outcome can be an `Allow`.
///
/// ```
/// use harness::action_auditor::{
///     ActionAuditor, FailClosedActionAuditor, RuleActionAuditor, ACTION_AUDIT_TIMEOUT,
/// };
/// use harness::leases::InMemoryLeaseStore;
/// use harness::windows::WindowSet;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// let rules = RuleActionAuditor::new(
///     Arc::new(WindowSet::default()),
///     Arc::new(InMemoryLeaseStore::default()),
///     chrono::Duration::minutes(10),
/// );
/// let auditor = FailClosedActionAuditor::new(rules);
/// assert_eq!(auditor.timeout(), ACTION_AUDIT_TIMEOUT);
/// assert_eq!(auditor.name(), "rule-action-auditor");
/// let quick = auditor.with_timeout(Duration::from_secs(5));
/// assert_eq!(quick.timeout(), Duration::from_secs(5));
/// ```
#[derive(Debug)]
pub struct FailClosedActionAuditor<A: ActionAuditor> {
    inner: A,
    timeout: Duration,
}

impl<A: ActionAuditor> FailClosedActionAuditor<A> {
    /// Wrap `inner` with the default [`ACTION_AUDIT_TIMEOUT`].
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            timeout: ACTION_AUDIT_TIMEOUT,
        }
    }

    /// Replace the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The configured timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[async_trait]
impl<A: ActionAuditor> ActionAuditor for FailClosedActionAuditor<A> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn review_action(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError> {
        match tokio::time::timeout(self.timeout, self.inner.review_action(review, context)).await {
            Ok(result) => result,
            Err(_) => Err(ActionAuditError::Timeout {
                timeout: self.timeout,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_auditor::rules::tests::review as sample_review;
    use crate::action_auditor::{ActionAuditLog, ActionGate};
    use crate::auditor::{Reason, ReasonCode, VerdictKind};
    use crate::effects::EffectClass;
    use crate::leases::LeaseContext;
    use std::sync::Arc;

    fn ctx() -> ActionContext<'static> {
        ActionContext {
            max_effect: EffectClass::Repository,
            window: None,
            lease: LeaseContext::default(),
            now: chrono::Utc::now(),
            task: "",
        }
    }

    enum Behaviour {
        Verdict(ActionVerdict),
        Error,
        Hang,
        Sleep(Duration),
    }

    struct Scripted(Behaviour);

    #[async_trait]
    impl ActionAuditor for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            match &self.0 {
                Behaviour::Verdict(v) => Ok(v.clone()),
                Behaviour::Error => Err(ActionAuditError::Model(
                    model::ModelError::ServiceUnavailable {
                        message: "down".to_string(),
                    },
                )),
                Behaviour::Hang => std::future::pending().await,
                Behaviour::Sleep(d) => {
                    tokio::time::sleep(*d).await;
                    Ok(ActionVerdict::Allow)
                }
            }
        }
    }

    const BOUND: Duration = Duration::from_secs(3600);

    async fn bounded<A: ActionAuditor>(a: &A) -> Result<ActionVerdict, ActionAuditError> {
        tokio::time::timeout(BOUND, a.review_action(&review(), &ctx()))
            .await
            .expect("auditor must time out on its own, not rely on the caller")
    }

    fn review() -> ActionReview {
        sample_review("push", EffectClass::Repository)
    }

    #[test]
    fn default_timeout_is_sixty_seconds_and_overridable() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Error));
        assert_eq!(a.timeout(), Duration::from_secs(60));
        assert_eq!(ACTION_AUDIT_TIMEOUT, Duration::from_secs(60));
        let a = a.with_timeout(Duration::from_secs(3));
        assert_eq!(a.timeout(), Duration::from_secs(3));
    }

    #[test]
    fn name_delegates_to_the_inner_auditor() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Error));
        assert_eq!(a.name(), "scripted");
    }

    #[tokio::test]
    async fn each_inner_verdict_passes_through_unchanged() {
        let reasons = vec![Reason::new(ReasonCode::Other, "r")];
        for verdict in [
            ActionVerdict::Allow,
            ActionVerdict::block(reasons.clone()),
            ActionVerdict::escalate(reasons.clone()),
        ] {
            let a = FailClosedActionAuditor::new(Scripted(Behaviour::Verdict(verdict.clone())));
            let got = a.review_action(&review(), &ctx()).await.unwrap();
            assert_eq!(got, verdict);
        }
    }

    #[tokio::test]
    async fn an_inner_error_is_an_error_never_an_allow() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Error));
        let err = a.review_action(&review(), &ctx()).await.unwrap_err();
        assert!(matches!(err, ActionAuditError::Model(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_inner_auditor_times_out_with_an_error() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Hang))
            .with_timeout(Duration::from_secs(7));
        let err = bounded(&a).await.unwrap_err();
        assert!(
            matches!(err, ActionAuditError::Timeout { timeout } if timeout == Duration::from_secs(7))
        );
        assert!(err.to_string().contains("7s"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_sub_second_timeout_is_reported_exactly() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Hang))
            .with_timeout(Duration::from_millis(250));
        let err = bounded(&a).await.unwrap_err();
        assert!(
            matches!(err, ActionAuditError::Timeout { timeout } if timeout == Duration::from_millis(250))
        );
        assert!(err.to_string().contains("250ms"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_within_the_timeout_is_kept() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Sleep(Duration::from_secs(5))))
            .with_timeout(Duration::from_secs(10));
        assert_eq!(
            a.review_action(&review(), &ctx()).await.unwrap(),
            ActionVerdict::Allow
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_after_the_timeout_is_discarded() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Sleep(Duration::from_secs(11))))
            .with_timeout(Duration::from_secs(10));
        assert!(bounded(&a).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn the_default_timeout_applies_when_none_is_set() {
        let a = FailClosedActionAuditor::new(Scripted(Behaviour::Hang));
        let started = tokio::time::Instant::now();
        assert!(bounded(&a).await.is_err());
        assert_eq!(started.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_maps_timeout_and_error_to_block() {
        for behaviour in [Behaviour::Hang, Behaviour::Error] {
            let a = FailClosedActionAuditor::new(Scripted(behaviour))
                .with_timeout(Duration::from_secs(1));
            let gate = ActionGate::new(Arc::new(a), ActionAuditLog::in_memory());
            let verdict = tokio::time::timeout(BOUND, gate.run_gate(&review(), &ctx()))
                .await
                .expect("gate must not wait on a hanging auditor beyond its timeout");
            assert_eq!(verdict.kind(), VerdictKind::Block);
            assert!(!verdict.is_allow());
        }
    }
}
