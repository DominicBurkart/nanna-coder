use super::{QueueStore, QueueStoreError, TaskQueue};
use crate::telemetry::TelemetrySystem;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Snapshot of the backlog for telemetry and health output.
///
/// ```
/// use harness::scheduler::QueueMetrics;
/// use harness::telemetry::TelemetrySystem;
///
/// let metrics = QueueMetrics {
///     queued: 3,
///     parked: 1,
///     running: 2,
///     oldest_age_seconds: Some(42.0),
///     dispatched_newest: 5,
///     dispatched_oldest: 4,
/// };
/// let telemetry = TelemetrySystem::new();
/// metrics.record(&telemetry);
/// assert_eq!(telemetry.get_buffered_metrics_count(), 6);
/// assert_eq!(metrics.to_json()["queued"], 3);
/// assert_eq!(
///     metrics.to_string(),
///     "queued=3 parked=1 running=2 oldest_age=42s dispatched_newest=5 dispatched_oldest=4"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueMetrics {
    /// Entries waiting for a slot, parked ones included.
    pub queued: usize,
    /// Entries whose `not_before` has not passed.
    pub parked: usize,
    /// Tasks occupying a slot.
    pub running: usize,
    /// Age of the oldest queued entry, if any.
    pub oldest_age_seconds: Option<f64>,
    /// Tasks dispatched by the newest cursor since the dispatcher started.
    pub dispatched_newest: u64,
    /// Tasks dispatched by the oldest cursor since the dispatcher started.
    pub dispatched_oldest: u64,
}

impl QueueMetrics {
    /// Depth metrics for a persisted queue that no dispatcher is serving
    /// (for example from the `health` command); running and dispatch counts
    /// are zero.
    pub fn from_store(store: &dyn QueueStore, now: DateTime<Utc>) -> Result<Self, QueueStoreError> {
        let queue = TaskQueue::from_entries(store.load()?);
        Ok(Self::from_queue(&queue, 0, 0, 0, now))
    }

    pub(crate) fn from_queue(
        queue: &TaskQueue,
        running: usize,
        dispatched_newest: u64,
        dispatched_oldest: u64,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            queued: queue.len(),
            parked: queue.parked(now),
            running,
            oldest_age_seconds: queue
                .oldest_age(now)
                .map(|age| age.num_milliseconds() as f64 / 1000.0),
            dispatched_newest,
            dispatched_oldest,
        }
    }

    /// Record the snapshot as gauges and counters on `telemetry`.
    pub fn record(&self, telemetry: &TelemetrySystem) {
        telemetry.record_gauge("nanna_queue_depth", self.queued as f64, vec![]);
        telemetry.record_gauge("nanna_queue_parked", self.parked as f64, vec![]);
        telemetry.record_gauge("nanna_queue_running", self.running as f64, vec![]);
        telemetry.record_gauge(
            "nanna_queue_oldest_age_seconds",
            self.oldest_age_seconds.unwrap_or(0.0),
            vec![],
        );
        telemetry.record_counter(
            "nanna_queue_dispatched_total",
            self.dispatched_newest as f64,
            vec![("side", "newest")],
        );
        telemetry.record_counter(
            "nanna_queue_dispatched_total",
            self.dispatched_oldest as f64,
            vec![("side", "oldest")],
        );
    }

    /// JSON form for health output and MCP metadata.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "queued": self.queued,
            "parked": self.parked,
            "running": self.running,
            "oldest_age_seconds": self.oldest_age_seconds,
            "dispatched_newest": self.dispatched_newest,
            "dispatched_oldest": self.dispatched_oldest,
        })
    }
}

impl fmt::Display for QueueMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "queued={} parked={} running={} oldest_age={} dispatched_newest={} dispatched_oldest={}",
            self.queued,
            self.parked,
            self.running,
            self.oldest_age_seconds
                .map_or_else(|| "none".to_string(), |age| format!("{age}s")),
            self.dispatched_newest,
            self.dispatched_oldest
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{InMemoryQueueStore, QueuedTask};
    use std::path::PathBuf;

    #[test]
    fn from_store_reports_depth_and_age() {
        let store = InMemoryQueueStore::default();
        let now = Utc::now();
        let mut old = QueuedTask::new("old", PathBuf::from("/r"), "HEAD", "m", 1);
        old.submitted_at = now - chrono::Duration::seconds(90);
        let parked = QueuedTask::new("parked", PathBuf::from("/r"), "HEAD", "m", 1)
            .with_not_before(Some(now + chrono::Duration::hours(1)));
        store.insert(&old).unwrap();
        store.insert(&parked).unwrap();
        let metrics = QueueMetrics::from_store(&store, now).unwrap();
        assert_eq!(metrics.queued, 2);
        assert_eq!(metrics.parked, 1);
        assert_eq!(metrics.running, 0);
        assert_eq!(metrics.oldest_age_seconds, Some(90.0));
        assert_eq!(metrics.to_json()["oldest_age_seconds"], 90.0);
    }

    #[test]
    fn empty_store_has_no_age() {
        let metrics = QueueMetrics::from_store(&InMemoryQueueStore::default(), Utc::now()).unwrap();
        assert_eq!(metrics.oldest_age_seconds, None);
        assert!(metrics.to_json()["oldest_age_seconds"].is_null());
        assert!(metrics.to_string().contains("oldest_age=none"));
        let telemetry = TelemetrySystem::new();
        metrics.record(&telemetry);
        assert_eq!(telemetry.get_buffered_metrics_count(), 6);
    }

    #[test]
    fn from_store_propagates_load_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        std::fs::write(&path, "garbage\n").unwrap();
        let store = crate::scheduler::JsonlQueueStore::open(&path).unwrap();
        assert!(QueueMetrics::from_store(&store, Utc::now()).is_err());
    }
}
