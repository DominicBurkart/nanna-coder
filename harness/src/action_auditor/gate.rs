//! The only path to a logged [`ActionVerdict`]: run a review through an
//! [`ActionAuditor`] and append the outcome to an [`ActionAuditLog`].

use super::{
    ActionAuditEvent, ActionAuditLog, ActionAuditSink, ActionAuditor, ActionContext, ActionReview,
    ActionVerdict,
};
use crate::auditor::{Reason, ReasonCode};
use std::fmt;
use std::sync::{Arc, OnceLock};

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
    /// because the auditor escalated it or it is the third denial the task has
    /// accumulated.
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
                "action escalated for human review: {}",
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
///     tool: "write_file".to_string(),
///     args: serde_json::json!({}),
///     effect_class: EffectClass::Workspace,
///     prior_actions: vec![],
/// };
/// let ctx = ActionContext {
///     max_effect: EffectClass::Workspace,
///     window: None,
///     lease: LeaseContext::default(),
///     now: chrono::Utc::now(),
///     task: "",
/// };
/// let verdict = gate.run_gate(&review, &ctx).await;
/// assert!(verdict.is_allow());
/// assert_eq!(gate.log().entries().unwrap().len(), 1);
/// # }
/// ```
pub struct ActionGate {
    auditor: Arc<dyn ActionAuditor>,
    log: ActionAuditLog,
    sink: OnceLock<Arc<dyn ActionAuditSink>>,
}

impl ActionGate {
    /// A gate over `auditor`, appending every reviewed outcome to `log`.
    pub fn new(auditor: Arc<dyn ActionAuditor>, log: ActionAuditLog) -> Self {
        Self {
            auditor,
            log,
            sink: OnceLock::new(),
        }
    }

    /// ```
    /// use harness::action_auditor::{ActionAuditLog, ActionGate, NullSink, RuleActionAuditor};
    /// use harness::leases::InMemoryLeaseStore;
    /// use harness::windows::WindowSet;
    /// use std::sync::Arc;
    ///
    /// let auditor = RuleActionAuditor::new(
    ///     Arc::new(WindowSet::default()),
    ///     Arc::new(InMemoryLeaseStore::default()),
    ///     chrono::Duration::minutes(10),
    /// );
    /// let gate = ActionGate::new(Arc::new(auditor), ActionAuditLog::in_memory());
    /// assert!(gate.set_sink(Arc::new(NullSink)));
    /// assert!(!gate.set_sink(Arc::new(NullSink)));
    /// ```
    pub fn set_sink(&self, sink: Arc<dyn ActionAuditSink>) -> bool {
        self.sink.set(sink).is_ok()
    }

    /// Name of the auditor behind this gate: the model for a
    /// [`ModelActionAuditor`](super::ModelActionAuditor), a fixed rule
    /// name otherwise.
    pub fn auditor_name(&self) -> &str {
        self.auditor.name()
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
        let decided = self.decide(review, context).await;
        self.emit(review, &decided);
        decided
    }

    fn emit(&self, review: &ActionReview, verdict: &ActionVerdict) {
        if let Some(sink) = self.sink.get() {
            sink.emit(ActionAuditEvent::new(review, verdict, self.auditor.name()));
        }
    }

    async fn decide(&self, review: &ActionReview, context: &ActionContext<'_>) -> ActionVerdict {
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
    use crate::auditor::{Reason, ReasonCode, VerdictKind};
    use crate::effects::EffectClass;
    use crate::leases::LeaseContext;
    use async_trait::async_trait;

    fn ctx() -> ActionContext<'static> {
        ActionContext {
            max_effect: EffectClass::Repository,
            window: None,
            lease: LeaseContext::default(),
            now: chrono::Utc::now(),
            task: "",
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
            .starts_with("action escalated for human review:"));
    }

    #[derive(Default)]
    struct Capture(std::sync::Mutex<Vec<ActionAuditEvent>>);

    impl ActionAuditSink for Capture {
        fn emit(&self, event: ActionAuditEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    impl Capture {
        fn events(&self) -> Vec<ActionAuditEvent> {
            self.0.lock().unwrap().clone()
        }
    }

    struct Decides(ActionVerdict);

    #[async_trait]
    impl ActionAuditor for Decides {
        fn name(&self) -> &str {
            "scripted-auditor"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            Ok(self.0.clone())
        }
    }

    fn captured(
        auditor: Arc<dyn ActionAuditor>,
        log: ActionAuditLog,
    ) -> (ActionGate, Arc<Capture>) {
        let gate = ActionGate::new(auditor, log);
        let capture = Arc::new(Capture::default());
        assert!(gate.set_sink(capture.clone()));
        (gate, capture)
    }

    #[tokio::test]
    async fn every_kind_of_decision_emits_exactly_one_event_with_its_codes() {
        let reason = Reason::new(ReasonCode::WindowClosed, "no window open");
        let cases = [
            (ActionVerdict::Allow, "allow", vec![]),
            (
                ActionVerdict::block(vec![reason.clone()]),
                "block",
                vec![ReasonCode::WindowClosed],
            ),
            (
                ActionVerdict::escalate(vec![reason]),
                "escalate",
                vec![ReasonCode::WindowClosed],
            ),
        ];
        for (verdict, name, codes) in cases {
            let (gate, capture) = captured(Arc::new(Decides(verdict)), ActionAuditLog::in_memory());
            let review = sample_review("sandbox_deploy", EffectClass::Sandbox);
            gate.run_gate(&review, &ctx()).await;
            let events = capture.events();
            assert_eq!(events.len(), 1, "{name}");
            assert_eq!(events[0].verdict, name);
            assert_eq!(events[0].reason_codes, codes);
            assert_eq!(events[0].effect_class, "sandbox");
            assert_eq!(events[0].tool, "sandbox_deploy");
            assert_eq!(events[0].auditor, "scripted-auditor");
        }
    }

    #[tokio::test]
    async fn an_auditor_error_emits_the_block_it_became() {
        let (gate, capture) = captured(Arc::new(AlwaysErrors), ActionAuditLog::in_memory());
        let review = sample_review("github_pr_status", EffectClass::Repository);
        gate.run_gate(&review, &ctx()).await;
        let events = capture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "block");
        assert_eq!(events[0].reason_codes, [ReasonCode::Other]);
    }

    #[tokio::test]
    async fn a_log_write_failure_emits_the_downgraded_block_not_the_allow() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let log = ActionAuditLog::file(blocker.join("audit.jsonl"));
        let (gate, capture) = captured(Arc::new(AlwaysAllows), log);
        let review = sample_review("github_pr_status", EffectClass::Repository);
        let verdict = gate.run_gate(&review, &ctx()).await;
        assert_eq!(verdict.kind(), VerdictKind::Block);
        let events = capture.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "block");
    }

    #[tokio::test]
    async fn every_decision_is_emitted_once_in_order_and_a_gate_without_a_sink_still_decides() {
        let (gate, capture) = captured(Arc::new(AlwaysAllows), ActionAuditLog::in_memory());
        for tool in ["a_tool", "b_tool", "c_tool"] {
            gate.run_gate(&sample_review(tool, EffectClass::Repository), &ctx())
                .await;
        }
        let tools: Vec<String> = capture.events().into_iter().map(|e| e.tool).collect();
        assert_eq!(tools, ["a_tool", "b_tool", "c_tool"]);
        let bare = ActionGate::new(Arc::new(AlwaysAllows), ActionAuditLog::in_memory());
        let review = sample_review("a_tool", EffectClass::Repository);
        assert!(bare.run_gate(&review, &ctx()).await.is_allow());
        assert_eq!(bare.log().entries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_event_carries_no_arguments_and_no_reason_detail() {
        let detail = Reason::new(ReasonCode::PromptInjection, "obey sk-live-SECRET-DETAIL");
        let decider = Decides(ActionVerdict::block(vec![detail]));
        let (gate, capture) = captured(Arc::new(decider), ActionAuditLog::in_memory());
        let mut review = sample_review("write_file", EffectClass::Repository);
        review.args = serde_json::json!({ "content": "sk-live-SECRET-ARGS" });
        review.identity = "x\ny sk-live-SECRET-ID".to_string();
        gate.run_gate(&review, &ctx()).await;
        let shown = serde_json::to_string(&capture.events()).unwrap();
        assert!(!shown.contains("SECRET-DETAIL") && !shown.contains("SECRET-ARGS"));
        assert!(!shown.contains(' ') && !shown.contains("\\n"));
    }

    #[tokio::test]
    async fn a_decision_reaches_the_telemetry_exporter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        let exporter = crate::telemetry::JsonlExporter::new(&path, 1 << 20);
        let telemetry =
            Arc::new(crate::telemetry::TelemetrySystem::new().add_exporter(Box::new(exporter)));
        let gate = ActionGate::new(Arc::new(AlwaysErrors), ActionAuditLog::in_memory());
        let sink = crate::action_auditor::TelemetrySink::new(Arc::clone(&telemetry));
        assert!(gate.set_sink(Arc::new(sink)));
        let review = sample_review("github_pr_status", EffectClass::Repository);
        gate.run_gate(&review, &ctx()).await;
        telemetry.export_all().await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let metric = lines.iter().find(|l| l.get("metric").is_some()).unwrap();
        assert_eq!(metric["metric"]["name"], "nanna_action_audit_total");
        assert_eq!(metric["metric"]["labels"]["verdict"], "block");
        assert_eq!(metric["metric"]["labels"]["tool"], "github_pr_status");
        let event = lines.iter().find(|l| l.get("event").is_some()).unwrap();
        assert_eq!(event["event"]["name"], "action_audit");
        assert_eq!(event["event"]["data"]["verdict"], "block");
    }

    #[tokio::test]
    async fn the_first_sink_wins_and_a_second_attachment_is_refused() {
        let (gate, first) = captured(Arc::new(AlwaysAllows), ActionAuditLog::in_memory());
        let second = Arc::new(Capture::default());
        assert!(!gate.set_sink(second.clone()));
        let review = sample_review("a_tool", EffectClass::Repository);
        gate.run_gate(&review, &ctx()).await;
        assert_eq!(first.events().len(), 1);
        assert!(second.events().is_empty());
    }
}
