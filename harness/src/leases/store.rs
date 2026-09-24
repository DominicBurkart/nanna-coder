use super::{LeaseKind, LeaseName};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

/// Capability that proves a [`Lease`] was granted to its holder. Only a
/// caller presenting the token can renew or release the lease, so a stale
/// clone from a previous grant cannot release someone else's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LeaseToken(String);

impl LeaseToken {
    fn fresh() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl fmt::Display for LeaseToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A granted lease: `holder` may act on `name` until `until`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub name: LeaseName,
    pub holder: String,
    pub acquired_at: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub token: LeaseToken,
}

impl Lease {
    /// Whether the lease has lapsed at `now` and may be reclaimed.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.until <= now
    }

    /// JSON form for observability. The token is deliberately omitted: it is
    /// the release capability, not a fact about the lease.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name.to_string(),
            "holder": self.holder,
            "acquired_at": self.acquired_at.to_rfc3339(),
            "until": self.until.to_rfc3339(),
        })
    }
}

/// Why a lease operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LeaseError {
    /// The lease, or a lease that excludes it, is held by someone else.
    ///
    /// `name` is the lease that stands in the way: the requested one, or the
    /// other `deploy:<repo>:*` lease when a repository already has a live
    /// deploy lease.
    #[error("lease `{name}` is held by `{by}` until {until}")]
    Held {
        name: LeaseName,
        by: String,
        until: DateTime<Utc>,
    },
    /// Renew or release of a lease the store no longer holds.
    #[error("lease `{name}` is not held")]
    NotHeld { name: LeaseName },
    /// Renew or release with a token that does not match the current grant.
    #[error("lease `{name}` is held under a different token")]
    TokenMismatch { name: LeaseName },
    /// A zero or negative TTL.
    #[error("lease TTL must be positive, got {0}")]
    NonPositiveTtl(Duration),
    /// [`required_leases`](super::required_leases) lacked the context field
    /// an effect needs.
    #[error("{effect} effect needs `{field}` in its lease context")]
    MissingContext {
        effect: super::Effect,
        field: &'static str,
    },
    /// An effect name did not match any [`Effect`](super::Effect).
    #[error("unknown effect `{0}` (expected one of: local, repository, sandbox, production)")]
    UnknownEffect(String),
    /// The persistent store could not be read or written.
    #[error("lease store I/O error: {0}")]
    Io(String),
    /// A persisted record could not be decoded.
    #[error("lease store record is not valid JSON: {0}")]
    Serde(String),
}

impl From<std::io::Error> for LeaseError {
    fn from(e: std::io::Error) -> Self {
        LeaseError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for LeaseError {
    fn from(e: serde_json::Error) -> Self {
        LeaseError::Serde(e.to_string())
    }
}

/// Named, TTL-bearing leases shared between every agent the harness runs.
///
/// A lease is granted to a `holder` (a task id) for `ttl` from `now`; the
/// caller supplies `now` so that tests and crash recovery can reason about
/// time explicitly. Acquiring a name the same holder already holds renews
/// it and returns the same token, so an executor may re-acquire per step.
/// An expired lease is reclaimed by the next acquirer, or in bulk by
/// [`expired`](Self::expired).
///
/// A repository may have at most one live `deploy:<repo>:*` lease: a second
/// deploy lease for the same repository is refused as
/// [`LeaseError::Held`] naming the lease in the way.
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::leases::{InMemoryLeaseStore, LeaseError, LeaseName, LeaseStore};
///
/// let store = InMemoryLeaseStore::default();
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let prod = LeaseName::deploy("example/repo", "prod");
///
/// let lease = store.acquire(&prod, "task-a", Duration::minutes(10), t0).unwrap();
/// assert_eq!(lease.until, t0 + Duration::minutes(10));
///
/// let refused = store.acquire(&prod, "task-b", Duration::minutes(10), t0).unwrap_err();
/// assert_eq!(refused, LeaseError::Held { name: prod.clone(), by: "task-a".into(), until: lease.until });
///
/// let renewed = store.renew(&lease, Duration::minutes(10), t0 + Duration::minutes(5)).unwrap();
/// assert_eq!(renewed.until, t0 + Duration::minutes(15));
/// assert_eq!(renewed.token, lease.token);
///
/// store.release(&renewed).unwrap();
/// assert!(store.acquire(&prod, "task-b", Duration::minutes(1), t0).is_ok());
/// assert_eq!(store.snapshot().unwrap().len(), 1);
/// ```
pub trait LeaseStore: Send + Sync {
    /// Grant `name` to `holder` for `ttl` from `now`, reclaiming an expired
    /// grant or renewing the holder's own.
    fn acquire(
        &self,
        name: &LeaseName,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError>;

    /// Extend `lease` to `now + ttl`. The token must match the current grant.
    fn renew(&self, lease: &Lease, ttl: Duration, now: DateTime<Utc>) -> Result<Lease, LeaseError>;

    /// Give `lease` back. The token must match the current grant.
    fn release(&self, lease: &Lease) -> Result<(), LeaseError>;

    /// Give back every lease `holder` has, returning them. Used when a task
    /// reaches a terminal state or is restored after a crash.
    fn release_all(&self, holder: &str) -> Result<Vec<Lease>, LeaseError>;

    /// Remove and return every lease that has lapsed at `now`.
    fn expired(&self, now: DateTime<Utc>) -> Result<Vec<Lease>, LeaseError>;

    /// Every lease currently recorded, expired ones included, in name order.
    fn snapshot(&self) -> Result<Vec<Lease>, LeaseError>;
}

/// Pure lease bookkeeping shared by every store implementation.
#[derive(Debug, Clone, Default)]
pub(super) struct LeaseTable {
    leases: BTreeMap<LeaseName, Lease>,
}

impl LeaseTable {
    pub(super) fn from_leases(leases: impl IntoIterator<Item = Lease>) -> Self {
        Self {
            leases: leases.into_iter().map(|l| (l.name.clone(), l)).collect(),
        }
    }

    pub(super) fn snapshot(&self) -> Vec<Lease> {
        self.leases.values().cloned().collect()
    }

    pub(super) fn insert(&mut self, lease: Lease) {
        self.leases.insert(lease.name.clone(), lease);
    }

    pub(super) fn remove(&mut self, name: &LeaseName) {
        self.leases.remove(name);
    }

    fn blocker(&self, name: &LeaseName, holder: &str, now: DateTime<Utc>) -> Option<&Lease> {
        let same_name = self.leases.get(name).filter(|l| !l.is_expired(now));
        if let Some(existing) = same_name {
            return (existing.holder != holder).then_some(existing);
        }
        if name.kind() != LeaseKind::Deploy {
            return None;
        }
        self.leases
            .range(LeaseName::deploy(name.repo(), "")..)
            .map(|(_, lease)| lease)
            .take_while(|l| l.name.kind() == LeaseKind::Deploy && l.name.repo() == name.repo())
            .find(|l| !l.is_expired(now))
    }

    pub(super) fn acquire(
        &mut self,
        name: &LeaseName,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError> {
        check_ttl(ttl)?;
        if let Some(blocker) = self.blocker(name, holder, now) {
            return Err(held(blocker));
        }
        let lease = match self.leases.get(name).filter(|l| !l.is_expired(now)) {
            Some(own) => Lease {
                until: now + ttl,
                ..own.clone()
            },
            None => Lease {
                name: name.clone(),
                holder: holder.to_string(),
                acquired_at: now,
                until: now + ttl,
                token: LeaseToken::fresh(),
            },
        };
        self.leases.insert(name.clone(), lease.clone());
        Ok(lease)
    }

    fn current(&self, lease: &Lease) -> Result<&Lease, LeaseError> {
        let current = self
            .leases
            .get(&lease.name)
            .ok_or_else(|| LeaseError::NotHeld {
                name: lease.name.clone(),
            })?;
        if current.token != lease.token {
            return Err(LeaseError::TokenMismatch {
                name: lease.name.clone(),
            });
        }
        Ok(current)
    }

    pub(super) fn renew(
        &mut self,
        lease: &Lease,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError> {
        check_ttl(ttl)?;
        let renewed = Lease {
            until: now + ttl,
            ..self.current(lease)?.clone()
        };
        self.leases.insert(renewed.name.clone(), renewed.clone());
        Ok(renewed)
    }

    pub(super) fn release(&mut self, lease: &Lease) -> Result<Lease, LeaseError> {
        let name = self.current(lease)?.name.clone();
        Ok(self.leases.remove(&name).expect("current lease is present"))
    }

    pub(super) fn release_all(&mut self, holder: &str) -> Vec<Lease> {
        self.drain(|l| l.holder == holder)
    }

    pub(super) fn expired(&mut self, now: DateTime<Utc>) -> Vec<Lease> {
        self.drain(|l| l.is_expired(now))
    }

    fn drain(&mut self, mut pred: impl FnMut(&Lease) -> bool) -> Vec<Lease> {
        let mut dropped = Vec::new();
        self.leases.retain(|_, lease| {
            let drop = pred(lease);
            if drop {
                dropped.push(lease.clone());
            }
            !drop
        });
        dropped
    }
}

fn check_ttl(ttl: Duration) -> Result<(), LeaseError> {
    if ttl <= Duration::zero() {
        return Err(LeaseError::NonPositiveTtl(ttl));
    }
    Ok(())
}

fn held(lease: &Lease) -> LeaseError {
    LeaseError::Held {
        name: lease.name.clone(),
        by: lease.holder.clone(),
        until: lease.until,
    }
}

/// Store that lives in process memory. Clones share the same table.
///
/// ```
/// use chrono::{Duration, Utc};
/// use harness::leases::{InMemoryLeaseStore, LeaseName, LeaseStore};
///
/// let store = InMemoryLeaseStore::default();
/// let now = Utc::now();
/// store.acquire(&LeaseName::branch("example/repo", "main"), "task-a", Duration::minutes(1), now).unwrap();
/// assert_eq!(store.clone().release_all("task-a").unwrap().len(), 1);
/// assert!(store.snapshot().unwrap().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryLeaseStore {
    table: Arc<Mutex<LeaseTable>>,
}

impl LeaseStore for InMemoryLeaseStore {
    fn acquire(
        &self,
        name: &LeaseName,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError> {
        self.table.lock().unwrap().acquire(name, holder, ttl, now)
    }

    fn renew(&self, lease: &Lease, ttl: Duration, now: DateTime<Utc>) -> Result<Lease, LeaseError> {
        self.table.lock().unwrap().renew(lease, ttl, now)
    }

    fn release(&self, lease: &Lease) -> Result<(), LeaseError> {
        self.table.lock().unwrap().release(lease).map(|_| ())
    }

    fn release_all(&self, holder: &str) -> Result<Vec<Lease>, LeaseError> {
        Ok(self.table.lock().unwrap().release_all(holder))
    }

    fn expired(&self, now: DateTime<Utc>) -> Result<Vec<Lease>, LeaseError> {
        Ok(self.table.lock().unwrap().expired(now))
    }

    fn snapshot(&self) -> Result<Vec<Lease>, LeaseError> {
        Ok(self.table.lock().unwrap().snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn ttl() -> Duration {
        Duration::minutes(10)
    }

    #[test]
    fn acquire_grants_then_refuses_other_holders() {
        let store = InMemoryLeaseStore::default();
        let name = LeaseName::branch("example/repo", "main");
        let lease = store.acquire(&name, "a", ttl(), t0()).unwrap();
        assert_eq!(lease.name, name);
        assert_eq!(lease.holder, "a");
        assert_eq!(lease.acquired_at, t0());
        assert_eq!(lease.until, t0() + ttl());
        assert!(!lease.is_expired(t0()));
        let err = store.acquire(&name, "b", ttl(), t0()).unwrap_err();
        assert_eq!(
            err,
            LeaseError::Held {
                name: name.clone(),
                by: "a".to_string(),
                until: lease.until
            }
        );
        assert_eq!(
            err.to_string(),
            "lease `branch:example/repo:main` is held by `a` until 2026-09-24 12:10:00 UTC"
        );
    }

    #[test]
    fn same_holder_reacquire_renews_with_same_token() {
        let store = InMemoryLeaseStore::default();
        let name = LeaseName::sandbox("example/repo", 3);
        let first = store.acquire(&name, "a", ttl(), t0()).unwrap();
        let again = store
            .acquire(&name, "a", ttl(), t0() + Duration::minutes(5))
            .unwrap();
        assert_eq!(again.token, first.token);
        assert_eq!(again.acquired_at, t0());
        assert_eq!(again.until, t0() + Duration::minutes(15));
        assert_eq!(store.snapshot().unwrap(), vec![again]);
    }

    #[test]
    fn expired_lease_is_reclaimed_by_next_acquirer_and_by_expired() {
        let store = InMemoryLeaseStore::default();
        let name = LeaseName::deploy("example/repo", "prod");
        let stale = store.acquire(&name, "a", ttl(), t0()).unwrap();
        let at_expiry = t0() + ttl();
        assert!(stale.is_expired(at_expiry));
        assert!(store.expired(t0()).unwrap().is_empty());
        let fresh = store.acquire(&name, "b", ttl(), at_expiry).unwrap();
        assert_eq!(fresh.holder, "b");
        assert_ne!(fresh.token, stale.token);
        assert_eq!(
            store.release(&stale).unwrap_err(),
            LeaseError::TokenMismatch { name: name.clone() }
        );
        assert_eq!(store.expired(at_expiry + ttl()).unwrap(), vec![fresh]);
        assert!(store.snapshot().unwrap().is_empty());
    }

    #[test]
    fn renew_and_release_require_the_matching_token() {
        let store = InMemoryLeaseStore::default();
        let name = LeaseName::branch("r", "main");
        let lease = store.acquire(&name, "a", ttl(), t0()).unwrap();
        let forged = Lease {
            token: LeaseToken::fresh(),
            ..lease.clone()
        };
        assert_eq!(
            store.renew(&forged, ttl(), t0()).unwrap_err(),
            LeaseError::TokenMismatch { name: name.clone() }
        );
        assert_eq!(
            store.release(&forged).unwrap_err(),
            LeaseError::TokenMismatch { name: name.clone() }
        );
        assert_eq!(store.snapshot().unwrap().len(), 1);
        store.release(&lease).unwrap();
        assert_eq!(
            store.release(&lease).unwrap_err(),
            LeaseError::NotHeld { name: name.clone() }
        );
        assert_eq!(
            store.renew(&lease, ttl(), t0()).unwrap_err().to_string(),
            "lease `branch:r:main` is not held"
        );
    }

    #[test]
    fn ttl_must_be_positive() {
        let store = InMemoryLeaseStore::default();
        let name = LeaseName::branch("r", "main");
        let err = store
            .acquire(&name, "a", Duration::zero(), t0())
            .unwrap_err();
        assert_eq!(err, LeaseError::NonPositiveTtl(Duration::zero()));
        assert!(err.to_string().contains("must be positive"));
        let lease = store.acquire(&name, "a", ttl(), t0()).unwrap();
        assert!(matches!(
            store.renew(&lease, Duration::seconds(-1), t0()),
            Err(LeaseError::NonPositiveTtl(_))
        ));
    }

    #[test]
    fn release_all_drops_only_the_holders_leases() {
        let store = InMemoryLeaseStore::default();
        store
            .acquire(&LeaseName::branch("r", "a"), "a", ttl(), t0())
            .unwrap();
        store
            .acquire(&LeaseName::branch("r", "b"), "a", ttl(), t0())
            .unwrap();
        let other = store
            .acquire(&LeaseName::branch("r", "c"), "b", ttl(), t0())
            .unwrap();
        let released = store.release_all("a").unwrap();
        assert_eq!(released.len(), 2);
        assert!(released.iter().all(|l| l.holder == "a"));
        assert_eq!(store.snapshot().unwrap(), vec![other]);
        assert!(store.release_all("nobody").unwrap().is_empty());
    }

    #[test]
    fn at_most_one_live_deploy_lease_per_repo() {
        let store = InMemoryLeaseStore::default();
        let prod = LeaseName::deploy("example/repo", "prod");
        let staging = LeaseName::deploy("example/repo", "staging");
        let lease = store.acquire(&prod, "a", ttl(), t0()).unwrap();
        let err = store.acquire(&staging, "b", ttl(), t0()).unwrap_err();
        assert_eq!(
            err,
            LeaseError::Held {
                name: prod.clone(),
                by: "a".to_string(),
                until: lease.until
            }
        );
        assert!(store.acquire(&staging, "a", ttl(), t0()).is_err());
        assert!(store
            .acquire(
                &LeaseName::deploy("example/other", "prod"),
                "b",
                ttl(),
                t0()
            )
            .is_ok());
        assert!(store
            .acquire(&LeaseName::branch("example/repo", "prod"), "b", ttl(), t0())
            .is_ok());
        let later = t0() + ttl();
        let replacement = store.acquire(&staging, "b", ttl(), later).unwrap();
        let expired = store.expired(later).unwrap();
        assert_eq!(expired.len(), 3);
        assert!(expired.contains(&lease));
        assert_eq!(store.snapshot().unwrap(), vec![replacement]);
    }

    #[test]
    fn lease_json_omits_the_token() {
        let store = InMemoryLeaseStore::default();
        let lease = store
            .acquire(&LeaseName::deploy("r", "prod"), "a", ttl(), t0())
            .unwrap();
        let json = lease.to_json();
        assert_eq!(json["name"], "deploy:r:prod");
        assert_eq!(json["holder"], "a");
        assert_eq!(json["acquired_at"], "2026-09-24T12:00:00+00:00");
        assert_eq!(json["until"], "2026-09-24T12:10:00+00:00");
        assert!(json.get("token").is_none());
        assert_eq!(lease.token.to_string().len(), 36);
        let io: LeaseError = std::io::Error::other("disk").into();
        assert_eq!(io.to_string(), "lease store I/O error: disk");
        let serde: LeaseError = serde_json::from_str::<Lease>("{").unwrap_err().into();
        assert!(serde
            .to_string()
            .starts_with("lease store record is not valid JSON"));
    }
}
