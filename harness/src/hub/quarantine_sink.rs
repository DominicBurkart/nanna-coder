//! The production [`QuarantineSink`]: pages the human plane about quarantines nothing else reports.

use super::send::{QuarantineEvent, QuarantineSink};
use crate::auditor::ReasonCode;
use crate::escalation::{Escalation, EscalationPlane, EscalationSource, Severity};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;

/// The repository named on every quarantine escalation. A pinned GitHub sink files in its own
/// repository whatever this says; the local record and a webhook carry it as a label.
pub const QUARANTINE_REPO: &str = "message-hub";

/// The first words of every quarantine escalation summary; the message id follows.
pub const QUARANTINE_SUMMARY: &str = "message held for human review:";

/// What the human is told to run.
pub const QUARANTINE_ACTION: &str = "run `nanna hub quarantine list`";

const NO_RUNTIME: &str = "no async runtime to deliver a quarantine escalation";
const NOT_DELIVERED: &str = "quarantine escalation delivery failed";

fn codes_of(codes: &[ReasonCode]) -> String {
    let named: Vec<String> = codes.iter().map(|code| format!("{code:?}")).collect();
    named.join(",")
}

fn ids_of(event: &QuarantineEvent) -> String {
    let named: Vec<String> = event
        .ids
        .iter()
        .map(|id| id.as_uuid().to_string())
        .collect();
    named.join(",")
}

/// The escalation for one event: ids, sender, codes, content hash and body length, never text.
///
/// ```
/// use harness::hub::lineage::SessionId;
/// use harness::hub::message::MessageId;
/// use harness::hub::quarantine_sink::escalation_of;
/// use harness::hub::send::{QuarantineEvent, QuarantineOrigin};
///
/// let event = QuarantineEvent {
///     origin: QuarantineOrigin::Recv,
///     ids: vec![MessageId::new()],
///     from: SessionId::from_task(harness::task::TaskId("a".to_string())),
///     codes: vec![],
///     content_hash: "abc".to_string(),
///     body_len: 3,
/// };
/// let escalation = escalation_of(&event);
/// assert!(escalation.summary.contains(&event.ids[0].as_uuid().to_string()));
/// ```
pub fn escalation_of(event: &QuarantineEvent) -> Escalation {
    let ids = ids_of(event);
    let evidence = vec![
        format!("message ids: {ids}"),
        format!("sender session: {}", event.from),
        format!("reason codes: {}", codes_of(&event.codes)),
        format!("content hash: {}", event.content_hash),
        format!("body length: {}", event.body_len),
    ];
    let summary = format!("{QUARANTINE_SUMMARY} {ids}");
    Escalation::new(
        Severity::Blocked,
        EscalationSource::Auditor,
        QUARANTINE_REPO,
        summary,
    )
    .with_evidence(evidence)
    .with_suggested_action(QUARANTINE_ACTION)
}

async fn deliver(
    plane: EscalationPlane,
    escalation: Escalation,
    pending: Arc<AtomicUsize>,
    failures: Arc<AtomicU64>,
) {
    let outcome = plane.escalator().escalate(escalation).await;
    if outcome.is_err() {
        failures.fetch_add(1, Ordering::SeqCst);
        tracing::error!("{NOT_DELIVERED}");
    }
    pending.fetch_sub(1, Ordering::SeqCst);
}

/// Pages the escalation plane about quarantines that no tool call reports: a quarantine found
/// at `recv` time (and, later, at the root inbox). A quarantine raised by `send` or by the
/// action gate already halts the sender through `ToolRegistry::execute`, which escalates once;
/// this sink skips those, so a human is never paged twice for one message.
///
/// The sink never changes what the hub decides: a failure to deliver is counted and logged with
/// a constant string, and the caller's verdict is untouched.
///
/// ```
/// use harness::escalation::{EscalationLog, EscalationPlane};
/// use harness::hub::quarantine_sink::EscalationQuarantineSink;
/// use std::sync::Arc;
///
/// let log = Arc::new(EscalationLog::in_memory());
/// let plane = EscalationPlane::from_lookup(log, &std::env::temp_dir(), &|_| None).unwrap();
/// let sink = EscalationQuarantineSink::from_plane(&plane);
/// assert_eq!(sink.failures(), 0);
/// ```
pub struct EscalationQuarantineSink {
    plane: EscalationPlane,
    pending: Arc<AtomicUsize>,
    failures: Arc<AtomicU64>,
}

impl EscalationQuarantineSink {
    /// A sink that escalates through `plane`.
    pub fn new(plane: EscalationPlane) -> Self {
        Self {
            plane,
            pending: Arc::new(AtomicUsize::new(0)),
            failures: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A sink over the escalation plane the host already runs.
    pub fn from_plane(plane: &EscalationPlane) -> Self {
        Self::new(plane.clone())
    }

    /// How many escalations could not be delivered or started since this sink was built.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::SeqCst)
    }

    /// Wait until every escalation this sink started has finished.
    pub async fn drain(&self) {
        while self.pending.load(Ordering::SeqCst) > 0 {
            tokio::task::yield_now().await;
        }
    }

    fn refuse(&self) -> String {
        self.failures.fetch_add(1, Ordering::SeqCst);
        tracing::error!("{NO_RUNTIME}");
        NO_RUNTIME.to_string()
    }
}

impl QuarantineSink for EscalationQuarantineSink {
    fn quarantined(&self, event: &QuarantineEvent) -> Result<(), String> {
        if event.origin.escalated_by_registry() {
            return Ok(());
        }
        let handle = Handle::try_current().map_err(|_| self.refuse())?;
        let escalation = escalation_of(event);
        let plane = self.plane.clone();
        self.pending.fetch_add(1, Ordering::SeqCst);
        let (pending, failures) = (Arc::clone(&self.pending), Arc::clone(&self.failures));
        handle.spawn(deliver(plane, escalation, pending, failures));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::lineage::SessionId;
    use crate::hub::message::MessageId;
    use crate::hub::send::QuarantineOrigin;
    use crate::test_support::escalation::{plane, CapturingSink};

    fn event(origin: QuarantineOrigin) -> QuarantineEvent {
        QuarantineEvent {
            origin,
            ids: vec![MessageId::new()],
            from: SessionId::from_task(crate::task::TaskId("sender".to_string())),
            codes: vec![ReasonCode::PromptInjection, ReasonCode::SecretLeak],
            content_hash: "feedfacefeedface".to_string(),
            body_len: 42,
        }
    }

    fn sink_over(captured: &Arc<CapturingSink>) -> EscalationQuarantineSink {
        EscalationQuarantineSink::new(plane(captured))
    }

    #[tokio::test]
    async fn a_recv_quarantine_is_one_blocked_auditor_escalation_with_ids_codes_hash_and_length() {
        let captured = CapturingSink::new(false);
        let sink = sink_over(&captured);
        let event = event(QuarantineOrigin::Recv);
        assert!(sink.quarantined(&event).is_ok());
        sink.drain().await;
        let seen = captured.escalations();
        assert_eq!(seen.len(), 1);
        let page = &seen[0];
        let id = event.ids[0].as_uuid().to_string();
        assert_eq!(page.severity, Severity::Blocked);
        assert_eq!(page.source, EscalationSource::Auditor);
        assert_eq!(page.repo, QUARANTINE_REPO);
        assert_eq!(page.summary, format!("{QUARANTINE_SUMMARY} {id}"));
        assert_eq!(page.suggested_action, QUARANTINE_ACTION);
        assert_eq!(page.evidence.len(), 5);
        assert_eq!(page.evidence[0], format!("message ids: {id}"));
        assert_eq!(page.evidence[1], format!("sender session: {}", event.from));
        assert_eq!(page.evidence[2], "reason codes: PromptInjection,SecretLeak");
        assert_eq!(page.evidence[3], "content hash: feedfacefeedface");
        assert_eq!(page.evidence[4], "body length: 42");
        assert_eq!(sink.failures(), 0);
    }

    #[tokio::test]
    async fn send_and_gate_quarantines_are_left_to_the_registry_escalation() {
        let captured = CapturingSink::new(false);
        let sink = sink_over(&captured);
        for origin in [QuarantineOrigin::Send, QuarantineOrigin::Gate] {
            assert!(sink.quarantined(&event(origin)).is_ok());
        }
        sink.drain().await;
        assert!(captured.escalations().is_empty());
        assert_eq!(sink.failures(), 0);
    }

    #[tokio::test]
    async fn distinct_messages_are_distinct_pages_and_the_same_message_collapses() {
        let captured = CapturingSink::new(false);
        let sink = sink_over(&captured);
        let first = event(QuarantineOrigin::Recv);
        let second = event(QuarantineOrigin::Recv);
        assert!(sink.quarantined(&first).is_ok());
        sink.drain().await;
        assert!(sink.quarantined(&second).is_ok());
        sink.drain().await;
        assert_eq!(captured.escalations().len(), 2);
        assert!(sink.quarantined(&first).is_ok());
        sink.drain().await;
        assert_eq!(captured.escalations().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_delivery_is_counted_logged_with_a_constant_and_never_an_error() {
        let (log, _guard) = crate::test_support::logs::capture();
        let captured = CapturingSink::new(true);
        let sink = sink_over(&captured);
        let event = event(QuarantineOrigin::Recv);
        assert!(sink.quarantined(&event).is_ok());
        sink.drain().await;
        assert_eq!(sink.failures(), 1);
        let logged = log.text();
        assert!(logged.contains(NOT_DELIVERED), "{logged}");
        assert!(!logged.contains(&event.content_hash), "{logged}");
        assert!(
            !logged.contains(&event.ids[0].as_uuid().to_string()),
            "{logged}"
        );
    }

    #[test]
    fn without_a_runtime_the_sink_refuses_loudly_with_a_constant_and_counts() {
        let captured = CapturingSink::new(false);
        let sink = sink_over(&captured);
        let refused = sink.quarantined(&event(QuarantineOrigin::Recv));
        assert_eq!(refused, Err(NO_RUNTIME.to_string()));
        assert_eq!(sink.failures(), 1);
        assert!(captured.escalations().is_empty());
        assert!(sink.quarantined(&event(QuarantineOrigin::Send)).is_ok());
        assert_eq!(sink.failures(), 1);
    }
}
