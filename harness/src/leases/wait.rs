use super::{acquire_all, Lease, LeaseError, LeaseName, LeaseStore};
use chrono::{DateTime, Duration, Utc};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// Future returned by [`Clock::sleep`].
pub type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Source of time for [`wait_for`], so tests can run contention scenarios
/// without real sleeps.
pub trait Clock: Send + Sync {
    /// Current instant.
    fn now(&self) -> DateTime<Utc>;
    /// Resolve once `duration` has passed.
    fn sleep(&self, duration: Duration) -> SleepFuture<'_>;
}

/// Wall-clock time and real sleeps.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        let delay = duration.to_std().unwrap_or_default();
        Box::pin(tokio::time::sleep(delay))
    }
}

/// Clock that only moves when asked: [`sleep`](Clock::sleep) advances it by
/// the requested duration and yields once so other tasks make progress.
/// Clones share the same instant.
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::leases::{Clock, SimulatedClock};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let clock = SimulatedClock::new(t0);
/// clock.sleep(Duration::seconds(30)).await;
/// assert_eq!(clock.now(), t0 + Duration::seconds(30));
/// assert_eq!(clock.sleeps(), vec![Duration::seconds(30)]);
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct SimulatedClock {
    now: Arc<Mutex<DateTime<Utc>>>,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

impl SimulatedClock {
    /// A clock reading `start`.
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
            sleeps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Move the clock forward by `duration`.
    pub fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap() += duration;
    }

    /// Every duration passed to [`sleep`](Clock::sleep), in order.
    pub fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl Clock for SimulatedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        self.sleeps.lock().unwrap().push(duration);
        self.advance(duration);
        Box::pin(tokio::task::yield_now())
    }
}

/// Exponential backoff between acquisition attempts: each wait doubles the
/// previous one up to `max`.
///
/// ```
/// use chrono::Duration;
/// use harness::leases::Backoff;
///
/// let backoff = Backoff::default();
/// assert_eq!(backoff.initial, Duration::milliseconds(100));
/// assert_eq!(backoff.next(Duration::seconds(4)), Duration::seconds(5));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// First wait after a refusal.
    pub initial: Duration,
    /// Longest wait between attempts.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::milliseconds(100),
            max: Duration::seconds(5),
        }
    }
}

impl Backoff {
    /// The wait that follows `current`.
    pub fn next(&self, current: Duration) -> Duration {
        (current * 2).min(self.max)
    }
}

/// How [`wait_for`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Every requested lease is held.
    Acquired(Vec<Lease>),
    /// The deadline passed while `blocked_on` was held by `held_by`; the
    /// caller should park until `until`, when that lease lapses at the
    /// latest, and try again.
    Parked {
        until: DateTime<Utc>,
        blocked_on: LeaseName,
        held_by: String,
    },
}

/// Acquire `names` for `holder`, retrying with `backoff` until `deadline`.
///
/// Each attempt is an [`acquire_all`], so no partial set is ever held while
/// waiting. A refusal after the deadline returns [`WaitOutcome::Parked`]
/// rather than an error so the scheduler can park the task instead of
/// failing it; every other error is returned as is. Waits never overshoot
/// the deadline or the blocking lease's expiry.
///
/// ```
/// use chrono::{Duration, TimeZone, Utc};
/// use harness::leases::{wait_for, Backoff, Clock, InMemoryLeaseStore, LeaseName, LeaseStore, SimulatedClock, WaitOutcome};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let store = InMemoryLeaseStore::default();
/// let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
/// let clock = SimulatedClock::new(t0);
/// let prod = LeaseName::deploy("example/repo", "prod");
/// let other = store.acquire(&prod, "task-a", Duration::hours(1), t0).unwrap();
///
/// let outcome = wait_for(&store, &[prod.clone()], "task-b", Duration::hours(1), t0 + Duration::seconds(3), &clock, Backoff::default()).await.unwrap();
/// assert_eq!(outcome, WaitOutcome::Parked { until: other.until, blocked_on: prod.clone(), held_by: "task-a".into() });
/// assert_eq!(clock.now(), t0 + Duration::seconds(3));
///
/// store.release(&other).unwrap();
/// let outcome = wait_for(&store, &[prod.clone()], "task-b", Duration::hours(1), clock.now(), &clock, Backoff::default()).await.unwrap();
/// assert!(matches!(outcome, WaitOutcome::Acquired(leases) if leases[0].holder == "task-b"));
/// # });
/// ```
pub async fn wait_for(
    store: &dyn LeaseStore,
    names: &[LeaseName],
    holder: &str,
    ttl: Duration,
    deadline: DateTime<Utc>,
    clock: &dyn Clock,
    backoff: Backoff,
) -> Result<WaitOutcome, LeaseError> {
    let mut delay = backoff.initial;
    loop {
        let now = clock.now();
        let (name, by, until) = match acquire_all(store, names, holder, ttl, now) {
            Ok(leases) => return Ok(WaitOutcome::Acquired(leases)),
            Err(LeaseError::Held { name, by, until }) => (name, by, until),
            Err(e) => return Err(e),
        };
        if now >= deadline {
            tracing::warn!(holder, lease = %name, held_by = %by, until = %until, "Lease wait deadline passed; parking");
            return Ok(WaitOutcome::Parked {
                until,
                blocked_on: name,
                held_by: by,
            });
        }
        let wait = delay.min(until - now).min(deadline - now);
        clock.sleep(wait).await;
        delay = backoff.next(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leases::InMemoryLeaseStore;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn prod() -> LeaseName {
        LeaseName::deploy("example/repo", "prod")
    }

    fn fast() -> Backoff {
        Backoff {
            initial: Duration::milliseconds(100),
            max: Duration::seconds(1),
        }
    }

    #[tokio::test]
    async fn acquires_immediately_when_free() {
        let store = InMemoryLeaseStore::default();
        let clock = SimulatedClock::new(t0());
        let names = [LeaseName::branch("r", "main"), prod()];
        let outcome = wait_for(
            &store,
            &names,
            "h",
            Duration::hours(1),
            t0(),
            &clock,
            fast(),
        )
        .await
        .unwrap();
        match outcome {
            WaitOutcome::Acquired(leases) => assert_eq!(leases.len(), 2),
            other => panic!("unexpected {other:?}"),
        }
        assert!(clock.sleeps().is_empty());
    }

    #[tokio::test]
    async fn backs_off_until_the_blocking_lease_lapses() {
        let store = InMemoryLeaseStore::default();
        let clock = SimulatedClock::new(t0());
        let held_until = t0() + Duration::milliseconds(1_500);
        store
            .acquire(&prod(), "other", Duration::milliseconds(1_500), t0())
            .unwrap();
        let deadline = t0() + Duration::hours(1);
        let outcome = wait_for(
            &store,
            &[prod()],
            "h",
            Duration::hours(1),
            deadline,
            &clock,
            fast(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, WaitOutcome::Acquired(ref l) if l[0].holder == "h"));
        assert_eq!(
            clock.sleeps(),
            vec![
                Duration::milliseconds(100),
                Duration::milliseconds(200),
                Duration::milliseconds(400),
                Duration::milliseconds(800),
            ]
        );
        assert_eq!(clock.now(), held_until);
    }

    #[tokio::test]
    async fn parks_when_the_deadline_passes() {
        let store = InMemoryLeaseStore::default();
        let clock = SimulatedClock::new(t0());
        let other = store
            .acquire(&prod(), "other", Duration::hours(1), t0())
            .unwrap();
        let deadline = t0() + Duration::milliseconds(250);
        let outcome = wait_for(
            &store,
            &[prod()],
            "h",
            Duration::hours(1),
            deadline,
            &clock,
            fast(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            WaitOutcome::Parked {
                until: other.until,
                blocked_on: prod(),
                held_by: "other".to_string()
            }
        );
        assert_eq!(
            clock.sleeps(),
            vec![Duration::milliseconds(100), Duration::milliseconds(150)]
        );
        assert_eq!(clock.now(), deadline);
        assert_eq!(store.snapshot().unwrap(), vec![other]);
    }

    #[tokio::test]
    async fn other_errors_propagate_without_waiting() {
        let store = InMemoryLeaseStore::default();
        let clock = SimulatedClock::new(t0());
        let err = wait_for(
            &store,
            &[prod()],
            "h",
            Duration::zero(),
            t0(),
            &clock,
            fast(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, LeaseError::NonPositiveTtl(Duration::zero()));
        assert!(clock.sleeps().is_empty());
    }

    #[tokio::test]
    async fn system_clock_reads_wall_time_and_sleeps() {
        let clock = SystemClock;
        let before = Utc::now();
        clock.sleep(Duration::milliseconds(1)).await;
        assert!(clock.now() >= before + Duration::milliseconds(1));
        clock.sleep(Duration::seconds(-1)).await;
        assert_eq!(
            Backoff::default().next(Duration::seconds(10)),
            Duration::seconds(5)
        );
    }

    struct Interval {
        holder: String,
        acquired_at: DateTime<Utc>,
        released_at: DateTime<Utc>,
    }

    fn assert_serialised(mut intervals: Vec<Interval>, expected: usize) {
        assert_eq!(intervals.len(), expected);
        intervals.sort_by_key(|i| i.acquired_at);
        for pair in intervals.windows(2) {
            assert!(
                pair[1].acquired_at >= pair[0].released_at,
                "{} acquired at {} before {} released at {}",
                pair[1].holder,
                pair[1].acquired_at,
                pair[0].holder,
                pair[0].released_at
            );
        }
    }

    async fn contend(
        store: Arc<dyn LeaseStore>,
        clock: Arc<dyn Clock>,
        holder: String,
        deadline: DateTime<Utc>,
        backoff: Backoff,
        inside: Arc<AtomicUsize>,
    ) -> Interval {
        let outcome = wait_for(
            &*store,
            &[prod()],
            &holder,
            Duration::hours(1),
            deadline,
            &*clock,
            backoff,
        )
        .await
        .unwrap();
        let WaitOutcome::Acquired(leases) = outcome else {
            panic!("{holder} was parked");
        };
        let acquired_at = clock.now();
        assert_eq!(
            inside.fetch_add(1, Ordering::SeqCst),
            0,
            "two holders inside"
        );
        clock.sleep(Duration::milliseconds(1)).await;
        assert_eq!(inside.fetch_sub(1, Ordering::SeqCst), 1);
        let released_at = clock.now();
        store.release(&leases[0]).unwrap();
        Interval {
            holder,
            acquired_at,
            released_at,
        }
    }

    #[tokio::test]
    async fn fifty_contenders_serialise_under_a_simulated_clock() {
        let store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::default());
        let clock = SimulatedClock::new(t0());
        let inside = Arc::new(AtomicUsize::new(0));
        let deadline = t0() + Duration::days(1);
        let mut handles = Vec::new();
        for n in 0..50 {
            let shared: Arc<dyn Clock> = Arc::new(clock.clone());
            handles.push(tokio::spawn(contend(
                Arc::clone(&store),
                shared,
                format!("task-{n}"),
                deadline,
                fast(),
                Arc::clone(&inside),
            )));
        }
        let mut intervals = Vec::new();
        for handle in handles {
            intervals.push(handle.await.unwrap());
        }
        assert_serialised(intervals, 50);
        assert!(clock.now() < deadline);
        assert!(store.snapshot().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fifty_real_tasks_serialise_on_the_system_clock() {
        let store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::default());
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let inside = Arc::new(AtomicUsize::new(0));
        let deadline = Utc::now() + Duration::seconds(60);
        let backoff = Backoff {
            initial: Duration::milliseconds(1),
            max: Duration::milliseconds(5),
        };
        let mut handles = Vec::new();
        for n in 0..50 {
            handles.push(tokio::spawn(contend(
                Arc::clone(&store),
                Arc::clone(&clock),
                format!("task-{n}"),
                deadline,
                backoff,
                Arc::clone(&inside),
            )));
        }
        let mut intervals = Vec::new();
        for handle in handles {
            intervals.push(handle.await.unwrap());
        }
        assert_serialised(intervals, 50);
        assert!(store.snapshot().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reversed_request_orders_never_deadlock() {
        let store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::default());
        let clock = SimulatedClock::new(t0());
        let deadline = t0() + Duration::hours(1);
        let forward = [LeaseName::branch("r", "main"), prod()];
        let reversed = [prod(), LeaseName::branch("r", "main")];
        let mut handles = Vec::new();
        for (holder, names) in [("a", forward), ("b", reversed)] {
            let store = Arc::clone(&store);
            let clock = clock.clone();
            handles.push(tokio::spawn(async move {
                let outcome = wait_for(
                    &*store,
                    &names,
                    holder,
                    Duration::hours(1),
                    deadline,
                    &clock,
                    fast(),
                )
                .await
                .unwrap();
                let WaitOutcome::Acquired(leases) = outcome else {
                    panic!("{holder} was parked");
                };
                let taken: Vec<LeaseName> = leases.iter().map(|l| l.name.clone()).collect();
                assert_eq!(taken, vec![prod(), LeaseName::branch("r", "main")]);
                clock.sleep(Duration::seconds(1)).await;
                store.release_all(holder).unwrap().len()
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap(), 2);
        }
        assert!(store.snapshot().unwrap().is_empty());
    }
}
