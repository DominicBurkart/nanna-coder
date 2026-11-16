//! A timeout and error wrapper that keeps a message auditor fail-closed.

use super::message_auditor::{
    rules_inbound, rules_outbound, InboundReview, MessageAuditor, OutboundReview, SharedOracle,
};
use super::{ActionAuditError, MessageVerdict, NullOracle, RuleMessageAuditor, RulesOutcome};
use crate::auditor::{Reason, ReasonCode};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Default time an inner message auditor gets before the review fails closed.
pub const MESSAGE_AUDIT_TIMEOUT: Duration = Duration::from_secs(30);

const FAILURE_DETAIL: &str = "the message auditor failed or timed out; human review required";

/// Wraps a [`MessageAuditor`] so that no review can hang, fail or silently pass.
///
/// When the inner auditor errors or outlives the timeout, the rules layer is
/// run again on the message and the result is never a `Deliver`: a clean
/// message becomes a `Quarantine` and a rules finding keeps its verdict, each
/// with a `ModelFailure` reason appended. A verdict the inner auditor would
/// have produced after the timeout is discarded.
///
/// ```
/// use harness::action_auditor::{
///     FailClosedMessageAuditor, MessageAuditor, RuleMessageAuditor, MESSAGE_AUDIT_TIMEOUT,
/// };
/// use std::time::Duration;
///
/// let auditor = FailClosedMessageAuditor::new(RuleMessageAuditor::new());
/// assert_eq!(auditor.timeout(), MESSAGE_AUDIT_TIMEOUT);
/// assert_eq!(auditor.name(), "rule-message-auditor");
/// let quick = auditor.with_timeout(Duration::from_millis(250));
/// assert_eq!(quick.timeout(), Duration::from_millis(250));
/// ```
pub struct FailClosedMessageAuditor<A: MessageAuditor> {
    inner: A,
    timeout: Duration,
    rules: RuleMessageAuditor,
    oracle: SharedOracle,
}

impl<A: MessageAuditor> std::fmt::Debug for FailClosedMessageAuditor<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailClosedMessageAuditor")
            .field("inner", &self.inner.name())
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<A: MessageAuditor> FailClosedMessageAuditor<A> {
    /// Wrap `inner` with the default [`MESSAGE_AUDIT_TIMEOUT`] and a [`NullOracle`].
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            timeout: MESSAGE_AUDIT_TIMEOUT,
            rules: RuleMessageAuditor::new(),
            oracle: Arc::new(NullOracle),
        }
    }

    /// Replace the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Use `oracle` for the rules re-run after a failure.
    ///
    /// ```
    /// use harness::action_auditor::{FailClosedMessageAuditor, NullOracle, RuleMessageAuditor};
    /// use std::sync::Arc;
    ///
    /// let _ = FailClosedMessageAuditor::new(RuleMessageAuditor::new()).with_oracle(Arc::new(NullOracle));
    /// ```
    pub fn with_oracle(mut self, oracle: SharedOracle) -> Self {
        self.oracle = oracle;
        self
    }

    /// The configured timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

fn degraded(rules: RulesOutcome) -> MessageVerdict {
    let failure = Reason::new(ReasonCode::ModelFailure, FAILURE_DETAIL);
    match rules.verdict {
        MessageVerdict::Deliver => MessageVerdict::Quarantine {
            reasons: vec![failure],
        },
        MessageVerdict::Block { mut reasons } => {
            reasons.push(failure);
            MessageVerdict::Block { reasons }
        }
        MessageVerdict::Quarantine { mut reasons } => {
            reasons.push(failure);
            MessageVerdict::Quarantine { reasons }
        }
    }
}

#[async_trait]
impl<A: MessageAuditor> MessageAuditor for FailClosedMessageAuditor<A> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn review_outbound(
        &self,
        review: &OutboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError> {
        match tokio::time::timeout(self.timeout, self.inner.review_outbound(review)).await {
            Ok(Ok(verdict)) => Ok(verdict),
            Ok(Err(_)) | Err(_) => Ok(degraded(rules_outbound(&self.rules, &*self.oracle, review))),
        }
    }

    async fn review_inbound(
        &self,
        review: &InboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError> {
        match tokio::time::timeout(self.timeout, self.inner.review_inbound(review)).await {
            Ok(Ok(verdict)) => Ok(verdict),
            Ok(Err(_)) | Err(_) => Ok(degraded(rules_inbound(&self.rules, &*self.oracle, review))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::message_auditor::fixtures::*;
    use super::super::{IdentifierOracle, ModelMessageAuditor};
    use super::*;
    use crate::effects::EffectClass;
    use crate::hub::lineage::SessionBinding;

    const BOUND: Duration = Duration::from_secs(3600);
    const SECS: Duration = Duration::from_secs(1);

    enum Behaviour {
        Verdict(MessageVerdict),
        Error,
        Hang,
        SleepThen(Duration, MessageVerdict),
    }

    struct Scripted(Behaviour);

    impl Scripted {
        async fn run(&self) -> Result<MessageVerdict, ActionAuditError> {
            match &self.0 {
                Behaviour::Verdict(v) => Ok(v.clone()),
                Behaviour::Error => Err(ActionAuditError::Model(
                    model::ModelError::ServiceUnavailable {
                        message: "down".to_string(),
                    },
                )),
                Behaviour::Hang => std::future::pending().await,
                Behaviour::SleepThen(d, v) => {
                    tokio::time::sleep(*d).await;
                    Ok(v.clone())
                }
            }
        }
    }

    #[async_trait]
    impl MessageAuditor for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn review_outbound(
            &self,
            _review: &OutboundReview<'_>,
        ) -> Result<MessageVerdict, ActionAuditError> {
            self.run().await
        }

        async fn review_inbound(
            &self,
            _review: &InboundReview<'_>,
        ) -> Result<MessageVerdict, ActionAuditError> {
            self.run().await
        }
    }

    fn wrap(b: Behaviour) -> FailClosedMessageAuditor<Scripted> {
        FailClosedMessageAuditor::new(Scripted(b))
    }

    fn outbound<'a>(sender: &'a SessionBinding, body: &'a str) -> OutboundReview<'a> {
        OutboundReview {
            sender,
            tool: "msg_send",
            recipients: &[],
            subject: "s",
            body,
            window: &[],
            thread: &[],
        }
    }

    fn only_failure(v: &MessageVerdict) -> bool {
        matches!(v, MessageVerdict::Quarantine { reasons }
            if reasons.len() == 1 && reasons[0].code == ReasonCode::ModelFailure)
    }

    fn has_failure(v: &MessageVerdict) -> bool {
        v.reasons().last().map(|r| r.code) == Some(ReasonCode::ModelFailure)
    }

    #[test]
    fn the_timeout_defaults_to_thirty_seconds_and_is_kept_exactly() {
        let a = wrap(Behaviour::Hang);
        assert_eq!(a.timeout(), Duration::from_secs(30));
        assert_eq!(MESSAGE_AUDIT_TIMEOUT, Duration::from_secs(30));
        let odd = Duration::new(1, 999);
        assert_eq!(a.with_timeout(odd).timeout(), odd);
        assert_eq!(wrap(Behaviour::Hang).name(), "scripted");
        assert!(format!("{:?}", wrap(Behaviour::Hang)).contains("scripted"));
    }

    #[tokio::test]
    async fn inner_verdicts_within_the_timeout_pass_through_unchanged() {
        let sender = implementer().await;
        let held = MessageVerdict::Quarantine {
            reasons: vec![Reason::new(ReasonCode::Other, "x")],
        };
        for v in [
            MessageVerdict::Deliver,
            held,
            MessageVerdict::Block {
                reasons: vec![Reason::new(ReasonCode::SecretLeak, "y")],
            },
        ] {
            let got = wrap(Behaviour::Verdict(v.clone()))
                .review_outbound(&outbound(&sender, "hello"))
                .await
                .unwrap();
            assert_eq!(got, v);
        }
    }

    #[tokio::test]
    async fn an_inner_error_becomes_a_quarantine_with_model_failure() {
        let sender = implementer().await;
        let got = wrap(Behaviour::Error)
            .review_outbound(&outbound(&sender, "hello"))
            .await
            .unwrap();
        assert!(only_failure(&got));
    }

    #[tokio::test]
    async fn an_inner_error_keeps_a_rules_block_and_appends_the_failure() {
        let sender = implementer().await;
        let body = format!("use {}", token());
        let got = wrap(Behaviour::Error)
            .review_outbound(&outbound(&sender, &body))
            .await
            .unwrap();
        assert!(got.is_block());
        assert_eq!(got.reasons()[0].code, ReasonCode::SecretLeak);
        assert!(has_failure(&got));
        assert!(!format!("{got:?}").contains(&token()));
    }

    #[tokio::test]
    async fn an_inner_error_keeps_a_rules_quarantine_and_appends_the_failure() {
        let sender = implementer().await;
        let got = wrap(Behaviour::Error)
            .review_outbound(&outbound(&sender, "my .env has\nPORT=1\nHOST=x\nMODE=y"))
            .await
            .unwrap();
        assert!(got.is_quarantine());
        assert_eq!(got.reasons().len(), 2);
        assert!(has_failure(&got));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_inner_auditor_times_out_into_a_quarantine() {
        let sender = implementer().await;
        let review = outbound(&sender, "hello");
        let auditor = wrap(Behaviour::Hang).with_timeout(Duration::from_millis(250));
        let started = tokio::time::Instant::now();
        let got = tokio::time::timeout(BOUND, auditor.review_outbound(&review))
            .await
            .expect("the wrapper must bound the review itself")
            .unwrap();
        assert!(only_failure(&got));
        assert_eq!(started.elapsed(), Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn a_deliver_that_arrives_after_the_timeout_is_discarded() {
        let sender = implementer().await;
        let review = outbound(&sender, "hello");
        let late = wrap(Behaviour::SleepThen(
            SECS + Duration::from_millis(1),
            MessageVerdict::Deliver,
        ))
        .with_timeout(SECS);
        let got = tokio::time::timeout(BOUND, late.review_outbound(&review))
            .await
            .unwrap()
            .unwrap();
        assert!(only_failure(&got));
        let in_time = wrap(Behaviour::SleepThen(
            SECS - Duration::from_millis(1),
            MessageVerdict::Deliver,
        ))
        .with_timeout(SECS);
        let got = tokio::time::timeout(BOUND, in_time.review_outbound(&review))
            .await
            .unwrap()
            .unwrap();
        assert!(got.is_deliver());
    }

    #[tokio::test(start_paused = true)]
    async fn the_inbound_path_fails_closed_too() {
        let recipient = shepherd().await;
        let m = thread_msg("a", "s", "hello");
        let mk = || InboundReview {
            recipient: &recipient,
            sender: scope("a", EffectClass::Ci, &[]),
            message: &m,
            outbound: None,
            thread: &[],
        };
        for b in [
            Behaviour::Error,
            Behaviour::Hang,
            Behaviour::SleepThen(2 * SECS, MessageVerdict::Deliver),
        ] {
            let got = tokio::time::timeout(BOUND, wrap(b).with_timeout(SECS).review_inbound(&mk()))
                .await
                .unwrap()
                .unwrap();
            assert!(only_failure(&got));
        }
        let leaking = thread_msg("a", "s", &format!("use {}", token()));
        let review = InboundReview {
            message: &leaking,
            ..mk()
        };
        let got = wrap(Behaviour::Error)
            .review_inbound(&review)
            .await
            .unwrap();
        assert!(got.is_block() && has_failure(&got));
        let ok = wrap(Behaviour::Verdict(MessageVerdict::Deliver))
            .review_inbound(&mk())
            .await
            .unwrap();
        assert!(ok.is_deliver());
    }

    #[tokio::test]
    async fn the_rules_rerun_uses_the_oracle_the_wrapper_was_given() {
        struct Known(String);
        impl IdentifierOracle for Known {
            fn is_known_git_object(&self, hex: &str) -> bool {
                hex == self.0
            }
            fn is_known_hub_id(&self, _id: &str) -> bool {
                false
            }
        }
        let sha = "0123456789abcdef".repeat(2) + "01234567";
        let body = format!("fixed in commit {sha}");
        let sender = implementer().await;
        let strict = wrap(Behaviour::Error)
            .review_outbound(&outbound(&sender, &body))
            .await
            .unwrap();
        assert!(strict.is_block());
        let vouched = wrap(Behaviour::Error)
            .with_oracle(Arc::new(Known(sha)))
            .review_outbound(&outbound(&sender, &body))
            .await
            .unwrap();
        assert!(only_failure(&vouched));
    }

    #[tokio::test]
    async fn the_inbound_rules_rerun_uses_the_oracle_the_wrapper_was_given() {
        struct Known(String);
        impl IdentifierOracle for Known {
            fn is_known_git_object(&self, hex: &str) -> bool {
                hex == self.0
            }
            fn is_known_hub_id(&self, _id: &str) -> bool {
                false
            }
        }
        let sha = "0123456789abcdef".repeat(2) + "01234567";
        let recipient = shepherd().await;
        let m = thread_msg("a", "s", &format!("fixed in commit {sha}"));
        let review = InboundReview {
            recipient: &recipient,
            sender: scope("a", EffectClass::Ci, &[]),
            message: &m,
            outbound: None,
            thread: &[],
        };
        let strict = wrap(Behaviour::Error)
            .review_inbound(&review)
            .await
            .unwrap();
        assert!(strict.is_block());
        let vouched = wrap(Behaviour::Error)
            .with_oracle(Arc::new(Known(sha)))
            .review_inbound(&review)
            .await
            .unwrap();
        assert!(only_failure(&vouched));
    }

    #[tokio::test(start_paused = true)]
    async fn wrapping_the_model_auditor_fails_closed_on_a_hung_or_failing_provider() {
        let sender = implementer().await;
        let review = outbound(&sender, "hello");
        for script in [Script::Hang, Script::Fail(PLANTED.to_string())] {
            let provider = ScriptedProvider::new(script);
            let auditor =
                FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider.clone(), "m"))
                    .with_timeout(SECS);
            let got = tokio::time::timeout(BOUND, auditor.review_outbound(&review))
                .await
                .unwrap()
                .unwrap();
            assert!(only_failure(&got));
            assert!(!format!("{got:?}").contains("harpoon"));
            assert_eq!(provider.calls(), 1);
        }
        let ok = ScriptedProvider::replying(r#"{"verdict":"deliver"}"#);
        let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(ok, "m"));
        assert!(auditor.review_outbound(&review).await.unwrap().is_deliver());
    }
}
