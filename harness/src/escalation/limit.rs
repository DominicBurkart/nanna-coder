use super::{DeliveryReceipt, Escalation, EscalationError, EscalationSink};
use crate::leases::Clock;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub const DEFAULT_REMOTE_CAP: usize = 10;

/// Caps how many escalations reach `inner` per severity inside a sliding
/// window, whatever their dedupe keys: distinct summaries mint distinct
/// keys, so the dedupe window alone does not bound a flood. Over the cap the
/// delivery fails with [`EscalationError::RateLimited`] without touching
/// `inner`; a failed attempt still uses budget.
///
/// ```
/// use harness::escalation::{Escalation, EscalationSink, EscalationSource, FanoutSink, RateLimitedSink, Severity};
/// use harness::leases::SystemClock;
/// use std::sync::Arc;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let sink = RateLimitedSink::new(Arc::new(FanoutSink(vec![])), Arc::new(SystemClock), 1, chrono::Duration::hours(1));
/// let first = Escalation::new(Severity::Blocked, EscalationSource::Auditor, "o/r", "one");
/// let second = Escalation::new(Severity::Blocked, EscalationSource::Auditor, "o/r", "two");
/// assert!(sink.deliver(&first).await.is_ok());
/// assert!(sink.deliver(&second).await.is_err());
/// # });
/// ```
pub struct RateLimitedSink {
    inner: Arc<dyn EscalationSink>,
    clock: Arc<dyn Clock>,
    max: usize,
    window: Duration,
    sent: Mutex<HashMap<String, VecDeque<DateTime<Utc>>>>,
}

impl RateLimitedSink {
    pub fn new(
        inner: Arc<dyn EscalationSink>,
        clock: Arc<dyn Clock>,
        max: usize,
        window: Duration,
    ) -> Self {
        Self {
            inner,
            clock,
            max,
            window,
            sent: Mutex::new(HashMap::new()),
        }
    }

    fn admit(&self, severity: &str) -> bool {
        let now = self.clock.now();
        let mut sent = self.sent.lock().unwrap_or_else(|p| p.into_inner());
        let bucket = sent.entry(severity.to_string()).or_default();
        while bucket.front().is_some_and(|t| now - *t >= self.window) {
            bucket.pop_front();
        }
        if bucket.len() >= self.max {
            return false;
        }
        bucket.push_back(now);
        true
    }
}

#[async_trait]
impl EscalationSink for RateLimitedSink {
    fn name(&self) -> &str {
        "remote"
    }

    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
        if !self.admit(&escalation.severity.to_string()) {
            tracing::warn!(severity = %escalation.severity, max = self.max, "remote escalation delivery capped; the escalation is only in the local record");
            return Err(EscalationError::RateLimited {
                max: self.max,
                window_secs: self.window.num_seconds(),
            });
        }
        self.inner.deliver(escalation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::sink::tests::RecordingSink;
    use crate::escalation::{EscalationSource, Severity};
    use crate::leases::SimulatedClock;
    use chrono::TimeZone;

    fn setup(max: usize, fail: bool) -> (RateLimitedSink, Arc<RecordingSink>, SimulatedClock) {
        let clock = SimulatedClock::new(Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0).unwrap());
        let inner = Arc::new(RecordingSink::new("rec", fail));
        let limited = RateLimitedSink::new(
            inner.clone(),
            Arc::new(clock.clone()),
            max,
            Duration::minutes(30),
        );
        (limited, inner, clock)
    }

    fn escalation(severity: Severity, summary: &str) -> Escalation {
        Escalation::new(severity, EscalationSource::Auditor, "example/repo", summary)
    }

    #[tokio::test]
    async fn the_cap_bounds_distinct_escalations_and_never_reaches_the_inner_sink_past_it() {
        let (limited, inner, _clock) = setup(3, false);
        for i in 0..3 {
            limited
                .deliver(&escalation(Severity::Blocked, &format!("path {i}")))
                .await
                .unwrap();
        }
        for i in 3..10 {
            let err = limited
                .deliver(&escalation(Severity::Blocked, &format!("path {i}")))
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    EscalationError::RateLimited {
                        max: 3,
                        window_secs: 1800
                    }
                ),
                "{err}"
            );
        }
        assert_eq!(inner.seen.lock().unwrap().len(), 3);
        assert_eq!(limited.name(), "remote");
    }

    #[tokio::test]
    async fn the_window_slides_and_reopens_exactly_when_it_has_elapsed() {
        let (limited, inner, clock) = setup(1, false);
        limited
            .deliver(&escalation(Severity::Blocked, "a"))
            .await
            .unwrap();
        clock.advance(Duration::minutes(29));
        assert!(limited
            .deliver(&escalation(Severity::Blocked, "b"))
            .await
            .is_err());
        clock.advance(Duration::minutes(1));
        limited
            .deliver(&escalation(Severity::Blocked, "c"))
            .await
            .unwrap();
        assert_eq!(inner.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn each_severity_has_its_own_budget_so_a_flood_cannot_starve_an_incident() {
        let (limited, inner, _clock) = setup(1, false);
        limited
            .deliver(&escalation(Severity::Blocked, "flood 1"))
            .await
            .unwrap();
        assert!(limited
            .deliver(&escalation(Severity::Blocked, "flood 2"))
            .await
            .is_err());
        limited
            .deliver(&escalation(Severity::Incident, "real incident"))
            .await
            .unwrap();
        assert_eq!(inner.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_attempt_still_uses_the_budget_so_retries_cannot_hammer_the_endpoint() {
        let (limited, inner, _clock) = setup(2, true);
        for _ in 0..2 {
            assert!(matches!(
                limited
                    .deliver(&escalation(Severity::Blocked, "x"))
                    .await
                    .unwrap_err(),
                EscalationError::UnknownHold(_)
            ));
        }
        assert!(matches!(
            limited
                .deliver(&escalation(Severity::Blocked, "x"))
                .await
                .unwrap_err(),
            EscalationError::RateLimited { .. }
        ));
        assert_eq!(inner.seen.lock().unwrap().len(), 2);
    }
}
