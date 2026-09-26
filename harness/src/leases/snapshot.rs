use super::{Lease, LeaseError, LeaseStore};
use crate::telemetry::TelemetrySystem;
use chrono::{DateTime, Utc};
use std::fmt;

/// Point-in-time view of every recorded lease, for `tasks/list` metadata,
/// `nanna health` and telemetry.
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::leases::{InMemoryLeaseStore, LeaseName, LeaseSnapshot, LeaseStore};
/// use harness::telemetry::TelemetrySystem;
///
/// let store = InMemoryLeaseStore::default();
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// store.acquire(&LeaseName::deploy("example/repo", "prod"), "task-a", Duration::minutes(5), t0).unwrap();
/// store.acquire(&LeaseName::branch("example/repo", "main"), "task-b", Duration::minutes(1), t0).unwrap();
///
/// let snapshot = LeaseSnapshot::from_store(&store, t0 + Duration::minutes(2)).unwrap();
/// assert_eq!((snapshot.held, snapshot.expired), (1, 1));
/// assert_eq!(snapshot.to_json()["leases"][0]["name"], "deploy:example/repo:prod");
/// assert_eq!(snapshot.to_string(), "held=1 expired=1");
///
/// let telemetry = TelemetrySystem::new();
/// snapshot.record(&telemetry);
/// assert_eq!(telemetry.get_buffered_metrics_count(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSnapshot {
    /// Every recorded lease, in name order.
    pub leases: Vec<Lease>,
    /// Leases still live at the snapshot instant.
    pub held: usize,
    /// Leases past their TTL that no one has reclaimed yet.
    pub expired: usize,
    /// Instant the counts were taken at.
    pub at: DateTime<Utc>,
}

impl LeaseSnapshot {
    /// Read `store` and count live and lapsed leases as of `now`.
    pub fn from_store(store: &dyn LeaseStore, now: DateTime<Utc>) -> Result<Self, LeaseError> {
        let leases = store.snapshot()?;
        let expired = leases.iter().filter(|l| l.is_expired(now)).count();
        Ok(Self {
            held: leases.len() - expired,
            expired,
            leases,
            at: now,
        })
    }

    /// Record the counts as gauges on `telemetry`.
    pub fn record(&self, telemetry: &TelemetrySystem) {
        telemetry.record_gauge("nanna_leases_held", self.held as f64, vec![]);
        telemetry.record_gauge("nanna_leases_expired", self.expired as f64, vec![]);
    }

    /// JSON form for health output and MCP metadata.
    pub fn to_json(&self) -> serde_json::Value {
        let leases: Vec<serde_json::Value> = self.leases.iter().map(Lease::to_json).collect();
        serde_json::json!({
            "held": self.held,
            "expired": self.expired,
            "at": self.at.to_rfc3339(),
            "leases": leases,
        })
    }
}

impl fmt::Display for LeaseSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "held={} expired={}", self.held, self.expired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leases::{InMemoryLeaseStore, LeaseName};
    use chrono::{Duration, TimeZone};

    #[test]
    fn counts_and_lists_every_lease() {
        let store = InMemoryLeaseStore::default();
        let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
        let empty = LeaseSnapshot::from_store(&store, t0).unwrap();
        assert_eq!(empty.held + empty.expired, 0);
        assert_eq!(empty.to_json()["leases"].as_array().unwrap().len(), 0);
        assert_eq!(empty.to_json()["at"], "2026-09-24T12:00:00+00:00");

        store
            .acquire(&LeaseName::sandbox("r", 1), "a", Duration::minutes(1), t0)
            .unwrap();
        store
            .acquire(
                &LeaseName::deploy("r", "prod"),
                "b",
                Duration::minutes(9),
                t0,
            )
            .unwrap();
        let later = t0 + Duration::minutes(5);
        let snapshot = LeaseSnapshot::from_store(&store, later).unwrap();
        assert_eq!(snapshot.held, 1);
        assert_eq!(snapshot.expired, 1);
        assert_eq!(snapshot.at, later);
        assert_eq!(snapshot.leases.len(), 2);
        let json = snapshot.to_json();
        assert_eq!(json["held"], 1);
        assert_eq!(json["expired"], 1);
        assert_eq!(json["leases"][0]["holder"], "b");
        assert_eq!(json["leases"][1]["name"], "sandbox:r:1");
        assert_eq!(snapshot.to_string(), "held=1 expired=1");
        let telemetry = TelemetrySystem::new();
        snapshot.record(&telemetry);
        assert_eq!(telemetry.get_buffered_metrics_count(), 2);
    }

    struct Broken;

    impl LeaseStore for Broken {
        fn acquire(
            &self,
            _name: &LeaseName,
            _holder: &str,
            _ttl: Duration,
            _now: DateTime<Utc>,
        ) -> Result<Lease, LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
        fn renew(
            &self,
            _lease: &Lease,
            _ttl: Duration,
            _now: DateTime<Utc>,
        ) -> Result<Lease, LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
        fn release(&self, _lease: &Lease) -> Result<(), LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
        fn release_all(&self, _holder: &str) -> Result<Vec<Lease>, LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
        fn expired(&self, _now: DateTime<Utc>) -> Result<Vec<Lease>, LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
        fn snapshot(&self) -> Result<Vec<Lease>, LeaseError> {
            Err(LeaseError::Io("broken".to_string()))
        }
    }

    #[test]
    fn store_failures_propagate() {
        let err = LeaseSnapshot::from_store(&Broken, Utc::now()).unwrap_err();
        assert_eq!(err, LeaseError::Io("broken".to_string()));
    }
}
