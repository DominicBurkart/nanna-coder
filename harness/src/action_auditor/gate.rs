//! The only path to a logged [`ActionVerdict`]: run a review through an
//! [`ActionAuditor`] and append the outcome to an [`ActionAuditLog`].

use super::{ActionAuditLog, ActionAuditor, ActionContext, ActionReview, ActionVerdict};
use crate::auditor::{Reason, ReasonCode};
use std::fmt;
use std::sync::Arc;

fn join_reasons(reasons: &[Reason]) -> String {
    reasons
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why a tool call was refused, handed back to
/// [`ToolRegistry::execute`](crate::tools::ToolRegistry::execute)'s caller
/// so the agent can adapt (`Block`) or the task can halt (`Escalate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDenied {
    /// The action must not run; the caller may try something else.
    Block {
        /// Why.
        reasons: Vec<Reason>,
    },
    /// The action must not run and the task should halt for human review,
    /// because this is the third denial the task has accumulated.
    Escalate {
        /// Why.
        reasons: Vec<Reason>,
    },
}

impl ActionDenied {
    /// The findings behind the denial.
    pub fn reasons(&self) -> &[Reason] {
        match self {
            ActionDenied::Block { reasons } | ActionDenied::Escalate { reasons } => reasons,
        }
    }

    /// Whether this denial should halt the task.
    pub fn is_escalation(&self) -> bool {
        matches!(self, ActionDenied::Escalate { .. })
    }
}

impl fmt::Display for ActionDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionDenied::Block { reasons } => {
                write!(f, "action blocked: {}", join_reasons(reasons))
            }
            ActionDenied::Escalate { reasons } => write!(
                f,
                "action escalated after repeated denials: {}",
                join_reasons(reasons)
            ),
        }
    }
}

/// Runs an [`ActionReview`] through an [`ActionAuditor`] and logs the
/// outcome.
///
/// Logging is not best-effort: a log write failure downgrades even an
/// `Allow` to a `Block`, so an effectful action can never run unlogged.
/// [`ActionGate`] holds no per-task state — the denial counter and the
/// per-task view of the log live on the [`ToolRegistry`](crate::tools::ToolRegistry)
/// that owns the task's reviews, so blocks from different tasks never add up
/// to one task's escalation.
///
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// use harness::action_auditor::{ActionAuditLog, ActionContext, ActionGate, ActionReview, RuleActionAuditor};
/// use harness::effects::EffectClass;
/// use harness::leases::{InMemoryLeaseStore, LeaseContext};
/// use harness::task::TaskId;
/// use harness::windows::WindowSet;
/// use std::sync::Arc;
///
/// let auditor = RuleActionAuditor::new(
///     Arc::new(WindowSet::default()),
///     Arc::new(InMemoryLeaseStore::default()),
///     chrono::Duration::minutes(10),
/// );
/// let gate = ActionGate::new(Arc::new(auditor), ActionAuditLog::in_memory());
///
/// let review = ActionReview {
///     identity: "rust-implementer".to_string(),
///     task_id: TaskId("t1".to_string()),
///     tool: "github_pr_status".to_string(),
///     args: serde_json::json!({}),
///     effect_class: EffectClass::Repository,
///     prior_actions: vec![],
/// };
/// let ctx = ActionContext {
///     max_effect: EffectClass::Repository,
///     window: None,
///     lease: LeaseContext::default(),
///     now: chrono::Utc::now(),
/// };
/// let verdict = gate.run_gate(&review, &ctx).await;
/// assert!(verdict.is_allow());
/// assert_eq!(gate.log().entries().unwrap().len(), 1);
/// # }
/// ```
pub struct ActionGate {
    auditor: Arc<dyn ActionAuditor>,
    log: ActionAuditLog,
}

impl ActionGate {
    /// A gate over `auditor`, appending every reviewed outcome to `log`.
    pub fn new(auditor: Arc<dyn ActionAuditor>, log: ActionAuditLog) -> Self {
        Self { auditor, log }
    }

    /// The audit log this gate appends every verdict to.
    pub fn log(&self) -> &ActionAuditLog {
        &self.log
    }

    /// Review `review`, log the outcome, and return the verdict.
    pub async fn run_gate(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> ActionVerdict {
        let verdict = match self.auditor.review_action(review, context).await {
            Ok(verdict) => verdict,
            Err(e) => {
                let reason = Reason::new(ReasonCode::Other, format!("action auditor error: {e}"));
                ActionVerdict::block(vec![reason])
            }
        };
        match self.log.append(review, &verdict) {
            Ok(()) => verdict,
            Err(e) => {
                let reason = Reason::new(
                    ReasonCode::Other,
                    format!("action audit log write failed: {e}"),
                );
                ActionVerdict::block(vec![reason])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_auditor::rules::tests::review as sample_review;
    use crate::action_auditor::ActionAuditError;
    use crate::auditor::VerdictKind;
    use crate::effects::EffectClass;
    use crate::leases::LeaseContext;
    use async_trait::async_trait;

    fn ctx() -> ActionContext<'static> {
        ActionContext {
            max_effect: EffectClass::Repository,
            window: None,
            lease: LeaseContext::default(),
            now: chrono::Utc::now(),
        }
    }

    struct AlwaysAllows;

    #[async_trait]
    impl ActionAuditor for AlwaysAllows {
        fn name(&self) -> &str {
            "always-allows"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            Ok(ActionVerdict::Allow)
        }
    }

    struct AlwaysErrors;

    #[async_trait]
    impl ActionAuditor for AlwaysErrors {
        fn name(&self) -> &str {
            "always-errors"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            Err(ActionAuditError::Model(
                model::ModelError::ServiceUnavailable {
                    message: "down".to_string(),
                },
            ))
        }
    }

    #[tokio::test]
    async fn allow_is_logged_and_returned() {
        let gate = ActionGate::new(Arc::new(AlwaysAllows), ActionAuditLog::in_memory());
        let review = sample_review("github_pr_status", EffectClass::Repository);
        let verdict = gate.run_gate(&review, &ctx()).await;
        assert!(verdict.is_allow());
        let entries = gate.log().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].review, review);
    }

    #[tokio::test]
    async fn auditor_error_becomes_a_logged_block() {
        let gate = ActionGate::new(Arc::new(AlwaysErrors), ActionAuditLog::in_memory());
        let review = sample_review("github_pr_status", EffectClass::Repository);
        let verdict = gate.run_gate(&review, &ctx()).await;
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert!(verdict.reasons()[0].detail.contains("action auditor error"));
        assert_eq!(gate.log().entries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_log_write_failure_downgrades_even_an_allow_to_a_block() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_directory = dir.path().join("blocker");
        std::fs::write(&not_a_directory, b"x").unwrap();
        let log = ActionAuditLog::file(not_a_directory.join("audit.jsonl"));
        let gate = ActionGate::new(Arc::new(AlwaysAllows), log);
        let review = sample_review("github_pr_status", EffectClass::Repository);
        let verdict = gate.run_gate(&review, &ctx()).await;
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert!(verdict.reasons()[0]
            .detail
            .contains("action audit log write failed"));
    }

    #[test]
    fn action_denied_displays_and_reports_escalation() {
        let reasons = vec![Reason::new(ReasonCode::Other, "x")];
        let block = ActionDenied::Block {
            reasons: reasons.clone(),
        };
        assert!(!block.is_escalation());
        assert_eq!(block.reasons(), &reasons[..]);
        assert_eq!(block.to_string(), "action blocked: other: x");

        let escalate = ActionDenied::Escalate { reasons };
        assert!(escalate.is_escalation());
        assert!(escalate
            .to_string()
            .starts_with("action escalated after repeated denials:"));
    }
}
