use super::{
    DeliveryReceipt, Escalation, EscalationError, EscalationLog, EscalationSink, Severity,
};
use crate::leases::Clock;
use chrono::Duration;
use std::sync::Arc;

/// Rate-limit window used when none is configured.
pub fn default_window() -> Duration {
    Duration::hours(1)
}

/// How [`Escalator::escalate`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// The sinks accepted occurrence `occurrence` of this key.
    Delivered {
        receipt: DeliveryReceipt,
        occurrence: u64,
    },
    /// An identical escalation was delivered inside the window; this one
    /// was counted and will be reported with the next delivery.
    Collapsed { occurrence: u64 },
}

/// The single path producers use: records the escalation in the
/// [`EscalationLog`], sets the incident hold for `incident` severity,
/// collapses repeats inside `window` into the counter, and delivers the
/// rest through `sink`.
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::escalation::{Escalation, EscalationLog, EscalationOutcome, EscalationSource, Escalator, FanoutSink, Severity};
/// use harness::leases::SimulatedClock;
/// use std::sync::Arc;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let clock = SimulatedClock::new(t0);
/// let log = Arc::new(EscalationLog::in_memory());
/// let escalator = Escalator::new(Arc::clone(&log), Arc::new(FanoutSink(vec![])), Arc::new(clock.clone()), Duration::minutes(10));
///
/// let incident = Escalation::new(Severity::Incident, EscalationSource::Rollout, "example/repo", "p99 breached");
/// assert!(matches!(escalator.escalate(incident.clone()).await.unwrap(), EscalationOutcome::Delivered { occurrence: 1, .. }));
/// assert!(log.production_held("example/repo"));
///
/// clock.advance(Duration::minutes(1));
/// assert_eq!(escalator.escalate(incident.clone()).await.unwrap(), EscalationOutcome::Collapsed { occurrence: 2 });
/// clock.advance(Duration::minutes(10));
/// assert!(matches!(escalator.escalate(incident).await.unwrap(), EscalationOutcome::Delivered { occurrence: 3, .. }));
/// # });
/// ```
pub struct Escalator {
    log: Arc<EscalationLog>,
    sink: Arc<dyn EscalationSink>,
    clock: Arc<dyn Clock>,
    window: Duration,
}

impl Escalator {
    pub fn new(
        log: Arc<EscalationLog>,
        sink: Arc<dyn EscalationSink>,
        clock: Arc<dyn Clock>,
        window: Duration,
    ) -> Self {
        Self {
            log,
            sink,
            clock,
            window,
        }
    }

    pub fn log(&self) -> &Arc<EscalationLog> {
        &self.log
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    /// Hand `escalation` to a human. The incident hold is recorded before
    /// delivery is attempted, so production stays parked even when every
    /// sink fails; a failed delivery is counted but not marked delivered,
    /// so the next identical escalation is sent again instead of collapsed.
    pub async fn escalate(
        &self,
        mut escalation: Escalation,
    ) -> Result<EscalationOutcome, EscalationError> {
        let now = self.clock.now();
        if escalation.severity == Severity::Incident {
            self.log.hold(&escalation, now)?;
        }
        let occurrence = self
            .log
            .occurrence(&escalation.dedupe_key(), now, self.window);
        escalation.occurrence = occurrence.number;
        if !occurrence.deliver {
            self.log.record(&escalation, now, false)?;
            tracing::info!(key = %escalation.dedupe_key(), occurrence = occurrence.number, "Escalation collapsed into counter");
            return Ok(EscalationOutcome::Collapsed {
                occurrence: occurrence.number,
            });
        }
        let result = self.sink.deliver(&escalation).await;
        self.log.record(&escalation, now, result.is_ok())?;
        Ok(EscalationOutcome::Delivered {
            receipt: result?,
            occurrence: occurrence.number,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::sink::tests::RecordingSink;
    use crate::escalation::{EscalationSource, FanoutSink};
    use crate::leases::SimulatedClock;
    use chrono::{TimeZone, Utc};

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn setup(fail: bool) -> (Escalator, Arc<RecordingSink>, SimulatedClock) {
        let clock = SimulatedClock::new(t0());
        let sink = Arc::new(RecordingSink::new("rec", fail));
        let escalator = Escalator::new(
            Arc::new(EscalationLog::in_memory()),
            sink.clone(),
            Arc::new(clock.clone()),
            Duration::minutes(30),
        );
        (escalator, sink, clock)
    }

    fn blocked(summary: &str) -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::ScopeDenials,
            "example/repo",
            summary,
        )
    }

    #[tokio::test]
    async fn repeats_inside_the_window_collapse_and_flush_after_it() {
        let (escalator, sink, clock) = setup(false);
        assert_eq!(escalator.window(), Duration::minutes(30));
        let first = escalator.escalate(blocked("denied: push")).await.unwrap();
        assert!(
            matches!(&first, EscalationOutcome::Delivered { occurrence: 1, receipt } if receipt.sink == "rec")
        );
        clock.advance(Duration::minutes(5));
        assert_eq!(
            escalator.escalate(blocked("denied: push")).await.unwrap(),
            EscalationOutcome::Collapsed { occurrence: 2 }
        );
        clock.advance(Duration::minutes(5));
        assert_eq!(
            escalator.escalate(blocked("denied: push")).await.unwrap(),
            EscalationOutcome::Collapsed { occurrence: 3 }
        );
        assert!(matches!(
            escalator.escalate(blocked("denied: merge")).await.unwrap(),
            EscalationOutcome::Delivered { occurrence: 1, .. }
        ));
        clock.advance(Duration::minutes(20));
        assert!(matches!(
            escalator.escalate(blocked("denied: push")).await.unwrap(),
            EscalationOutcome::Delivered { occurrence: 4, .. }
        ));
        let seen = sink.seen.lock().unwrap();
        assert_eq!(
            seen.iter()
                .map(|e| (e.summary.as_str(), e.occurrence))
                .collect::<Vec<_>>(),
            vec![
                ("denied: push", 1),
                ("denied: merge", 1),
                ("denied: push", 4)
            ]
        );
        let states = escalator.log().states();
        assert_eq!(states.iter().map(|s| s.count).sum::<u64>(), 5);
        assert!(!escalator.log().production_held("example/repo"));
    }

    #[tokio::test]
    async fn failed_delivery_is_counted_but_retried_next_time() {
        let (escalator, sink, clock) = setup(true);
        let err = escalator.escalate(blocked("x")).await.unwrap_err();
        assert!(matches!(err, EscalationError::UnknownHold(_)));
        clock.advance(Duration::seconds(1));
        let err = escalator.escalate(blocked("x")).await.unwrap_err();
        assert!(matches!(err, EscalationError::UnknownHold(_)));
        assert_eq!(
            sink.seen
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.occurrence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(escalator.log().states()[0].last_delivered, None);
    }

    #[tokio::test]
    async fn incident_sets_hold_even_when_delivery_fails_and_only_resolve_clears_it() {
        let (escalator, _sink, _clock) = setup(true);
        let incident = Escalation::new(
            Severity::Incident,
            EscalationSource::Incident,
            "example/repo",
            "down",
        )
        .with_id("inc-1");
        assert!(escalator.escalate(incident).await.is_err());
        assert!(escalator.log().production_held("example/repo"));
        assert!(!escalator.log().production_held("example/other"));
        assert_eq!(
            escalator.log().resolve("inc-1", t0()).unwrap().summary,
            "down"
        );
        assert!(!escalator.log().production_held("example/repo"));
    }

    #[tokio::test]
    async fn log_write_failure_surfaces_before_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.jsonl");
        let log = Arc::new(EscalationLog::open(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        let escalator = Escalator::new(
            log,
            Arc::new(FanoutSink(vec![])),
            Arc::new(SimulatedClock::new(t0())),
            default_window(),
        );
        let err = escalator
            .escalate(Escalation::new(
                Severity::Incident,
                EscalationSource::Manual,
                "r",
                "s",
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, EscalationError::Io(_)));
        let err = escalator
            .escalate(Escalation::new(
                Severity::Info,
                EscalationSource::Manual,
                "r",
                "s",
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, EscalationError::Io(_)));
        assert_eq!(default_window(), Duration::hours(1));
    }
}
