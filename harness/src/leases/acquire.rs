use super::{Lease, LeaseError, LeaseName, LeaseStore};
use chrono::{DateTime, Duration, Utc};

/// Acquire every name in `names` for `holder`, in the global order
/// (`deploy < branch < sandbox < paths`, then lexically), releasing whatever
/// was taken if any acquisition fails.
///
/// Because every holder takes leases in the same order and never waits
/// while holding a partial set, two holders cannot deadlock on each other.
/// Duplicate names are acquired once. The returned leases are in the same
/// order they were taken.
///
/// ```
/// use chrono::{Duration, Utc};
/// use harness::leases::{acquire_all, InMemoryLeaseStore, LeaseError, LeaseName, LeaseStore};
///
/// let store = InMemoryLeaseStore::default();
/// let now = Utc::now();
/// let names = [LeaseName::branch("example/repo", "main"), LeaseName::deploy("example/repo", "prod")];
///
/// let leases = acquire_all(&store, &names, "task-a", Duration::minutes(5), now).unwrap();
/// assert_eq!(leases[0].name, names[1]);
/// assert_eq!(leases[1].name, names[0]);
///
/// let refused = acquire_all(&store, &names, "task-b", Duration::minutes(5), now).unwrap_err();
/// assert!(matches!(refused, LeaseError::Held { .. }));
/// assert!(store.snapshot().unwrap().iter().all(|l| l.holder == "task-a"));
/// ```
pub fn acquire_all(
    store: &dyn LeaseStore,
    names: &[LeaseName],
    holder: &str,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Result<Vec<Lease>, LeaseError> {
    let mut ordered = names.to_vec();
    ordered.sort();
    ordered.dedup();
    let mut taken = Vec::with_capacity(ordered.len());
    for name in &ordered {
        match store.acquire(name, holder, ttl, now) {
            Ok(lease) => taken.push(lease),
            Err(e) => {
                roll_back(store, &taken);
                return Err(e);
            }
        }
    }
    Ok(taken)
}

fn roll_back(store: &dyn LeaseStore, taken: &[Lease]) {
    for lease in taken.iter().rev() {
        if let Err(e) = store.release(lease) {
            tracing::error!(lease = %lease.name, error = %e, "Failed to release lease while rolling back a partial acquisition");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leases::InMemoryLeaseStore;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn ttl() -> Duration {
        Duration::minutes(10)
    }

    #[test]
    fn acquires_in_global_order_and_dedups() {
        let store = InMemoryLeaseStore::default();
        let names = [
            LeaseName::paths("r", &["x"]),
            LeaseName::branch("r", "main"),
            LeaseName::deploy("r", "prod"),
            LeaseName::branch("r", "main"),
            LeaseName::sandbox("r", 1),
        ];
        let leases = acquire_all(&store, &names, "h", ttl(), t0()).unwrap();
        let taken: Vec<LeaseName> = leases.iter().map(|l| l.name.clone()).collect();
        assert_eq!(
            taken,
            vec![
                LeaseName::deploy("r", "prod"),
                LeaseName::branch("r", "main"),
                LeaseName::sandbox("r", 1),
                LeaseName::paths("r", &["x"]),
            ]
        );
        assert_eq!(store.snapshot().unwrap().len(), 4);
        assert!(acquire_all(&store, &[], "h", ttl(), t0())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failure_releases_everything_already_taken() {
        let store = InMemoryLeaseStore::default();
        let branch = LeaseName::branch("r", "main");
        let deploy = LeaseName::deploy("r", "prod");
        let other = store.acquire(&branch, "other", ttl(), t0()).unwrap();
        let err =
            acquire_all(&store, &[branch.clone(), deploy.clone()], "h", ttl(), t0()).unwrap_err();
        assert_eq!(
            err,
            LeaseError::Held {
                name: branch,
                by: "other".to_string(),
                until: other.until
            }
        );
        assert_eq!(store.snapshot().unwrap(), vec![other]);
        assert!(store.acquire(&deploy, "third", ttl(), t0()).is_ok());
    }

    struct ReleaseFails(InMemoryLeaseStore);

    impl LeaseStore for ReleaseFails {
        fn acquire(
            &self,
            name: &LeaseName,
            holder: &str,
            ttl: Duration,
            now: DateTime<Utc>,
        ) -> Result<Lease, LeaseError> {
            self.0.acquire(name, holder, ttl, now)
        }
        fn renew(
            &self,
            lease: &Lease,
            ttl: Duration,
            now: DateTime<Utc>,
        ) -> Result<Lease, LeaseError> {
            self.0.renew(lease, ttl, now)
        }
        fn release(&self, _lease: &Lease) -> Result<(), LeaseError> {
            Err(LeaseError::Io("read-only".to_string()))
        }
        fn release_all(&self, holder: &str) -> Result<Vec<Lease>, LeaseError> {
            self.0.release_all(holder)
        }
        fn expired(&self, now: DateTime<Utc>) -> Result<Vec<Lease>, LeaseError> {
            self.0.expired(now)
        }
        fn snapshot(&self) -> Result<Vec<Lease>, LeaseError> {
            self.0.snapshot()
        }
    }

    #[test]
    fn rollback_failure_keeps_the_original_error() {
        let store = ReleaseFails(InMemoryLeaseStore::default());
        let branch = LeaseName::branch("r", "main");
        let deploy = LeaseName::deploy("r", "prod");
        store.acquire(&branch, "other", ttl(), t0()).unwrap();
        let err = acquire_all(&store, &[deploy, branch], "h", ttl(), t0()).unwrap_err();
        assert!(matches!(err, LeaseError::Held { .. }));
        assert_eq!(store.snapshot().unwrap().len(), 2);
    }

    #[test]
    fn non_held_errors_propagate_after_rollback() {
        let store = InMemoryLeaseStore::default();
        let names = [LeaseName::deploy("r", "prod")];
        let err = acquire_all(&store, &names, "h", Duration::zero(), t0()).unwrap_err();
        assert_eq!(err, LeaseError::NonPositiveTtl(Duration::zero()));
        assert!(store.snapshot().unwrap().is_empty());
    }
}
