use super::error::HubError;
use super::lineage::{RootId, SessionId};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};

/// The longest span the per-sender rate limit counts over.
pub const SENDER_WINDOW_SECS: i64 = 600;

/// The longest span the per-root rate limit counts over.
pub const ROOT_WINDOW_SECS: i64 = 3600;

/// The hub's size, rate and fan-out limits. `session_idle_secs` caps the time one session may spend
/// blocked inside `msg_recv`, summed over every call since the hub started.
///
/// ```
/// use harness::hub::limits::HubLimits;
///
/// let limits = HubLimits::default();
/// assert_eq!(limits.body_max, 8192);
/// assert_eq!(limits.per_sender_per_10m, 30);
/// assert_eq!(limits.per_root_per_hour, 2000);
/// assert_eq!(limits.session_idle_secs, 1800);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubLimits {
    pub body_max: usize,
    pub subject_max: usize,
    pub per_sender_per_10m: u32,
    pub inbox_max: usize,
    pub per_root_per_hour: u32,
    pub fanout_max: usize,
    pub thread_depth_max: u8,
    pub session_idle_secs: u32,
}

impl Default for HubLimits {
    fn default() -> Self {
        Self {
            body_max: 8192,
            subject_max: 120,
            per_sender_per_10m: 30,
            inbox_max: 256,
            per_root_per_hour: 2000,
            fanout_max: 20,
            thread_depth_max: 32,
            session_idle_secs: 1800,
        }
    }
}

#[derive(Default)]
pub(crate) struct Windows {
    sender: HashMap<SessionId, VecDeque<DateTime<Utc>>>,
    root: HashMap<RootId, VecDeque<DateTime<Utc>>>,
    reserved: HashMap<SessionId, usize>,
}

fn prune(times: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>, secs: i64) {
    let floor = now - Duration::seconds(secs);
    while times.front().is_some_and(|oldest| *oldest <= floor) {
        times.pop_front();
    }
}

fn live(
    times: Option<&VecDeque<DateTime<Utc>>>,
    now: DateTime<Utc>,
    secs: i64,
) -> VecDeque<DateTime<Utc>> {
    let floor = now - Duration::seconds(secs);
    let all = times.cloned().unwrap_or_default();
    all.into_iter().filter(|at| *at > floor).collect()
}

fn retry_after(
    times: &VecDeque<DateTime<Utc>>,
    now: DateTime<Utc>,
    secs: i64,
) -> std::time::Duration {
    let oldest = times.front().copied().unwrap_or(now);
    (oldest + Duration::seconds(secs) - now)
        .to_std()
        .unwrap_or_default()
}

impl Windows {
    #[cfg(test)]
    pub(crate) fn sender_slots(&self, sender: &SessionId) -> usize {
        self.sender.get(sender).map_or(0, VecDeque::len)
    }

    #[cfg(test)]
    pub(crate) fn tracked(&self) -> (usize, usize) {
        (self.sender.len(), self.root.len())
    }

    pub(crate) fn held(&self, session: &SessionId) -> usize {
        self.reserved.get(session).copied().unwrap_or(0)
    }

    pub(crate) fn admit(
        &mut self,
        now: DateTime<Utc>,
        sender: &SessionId,
        root: &RootId,
        limits: &HubLimits,
    ) -> Result<(), HubError> {
        self.sender.retain(|_, times| {
            prune(times, now, SENDER_WINDOW_SECS);
            !times.is_empty()
        });
        self.root.retain(|_, times| {
            prune(times, now, ROOT_WINDOW_SECS);
            !times.is_empty()
        });
        let own = self.sender.entry(sender.clone()).or_default();
        let shared = self.root.entry(root.clone()).or_default();
        if own.len() >= limits.per_sender_per_10m as usize {
            return Err(HubError::RateLimited(retry_after(
                own,
                now,
                SENDER_WINDOW_SECS,
            )));
        }
        if shared.len() >= limits.per_root_per_hour as usize {
            return Err(HubError::RateLimited(retry_after(
                shared,
                now,
                ROOT_WINDOW_SECS,
            )));
        }
        own.push_back(now);
        shared.push_back(now);
        Ok(())
    }

    pub(crate) fn check(
        &self,
        now: DateTime<Utc>,
        sender: &SessionId,
        root: &RootId,
        limits: &HubLimits,
    ) -> Result<(), HubError> {
        let own = live(self.sender.get(sender), now, SENDER_WINDOW_SECS);
        let shared = live(self.root.get(root), now, ROOT_WINDOW_SECS);
        if own.len() >= limits.per_sender_per_10m as usize {
            return Err(HubError::RateLimited(retry_after(
                &own,
                now,
                SENDER_WINDOW_SECS,
            )));
        }
        if shared.len() >= limits.per_root_per_hour as usize {
            return Err(HubError::RateLimited(retry_after(
                &shared,
                now,
                ROOT_WINDOW_SECS,
            )));
        }
        Ok(())
    }

    pub(crate) fn reserve(&mut self, recipients: &[SessionId]) {
        for id in recipients {
            *self.reserved.entry(id.clone()).or_default() += 1;
        }
    }

    pub(crate) fn release(&mut self, recipients: &[SessionId]) {
        for id in recipients {
            if let Some(count) = self.reserved.get_mut(id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.reserved.remove(id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskId;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn id(name: &str) -> SessionId {
        SessionId::from_task(TaskId(name.into()))
    }

    #[test]
    fn check_never_consumes_a_slot_and_matches_admit() {
        let limits = HubLimits {
            per_sender_per_10m: 2,
            per_root_per_hour: 3,
            ..HubLimits::default()
        };
        let (a, b, root) = (id("a"), id("b"), RootId::mcp(uuid::Uuid::nil()));
        let mut windows = Windows::default();
        for _ in 0..5 {
            assert!(windows.check(at(0), &a, &root, &limits).is_ok());
        }
        assert_eq!(windows.tracked(), (0, 0));
        windows.admit(at(0), &a, &root, &limits).unwrap();
        windows.admit(at(1), &a, &root, &limits).unwrap();
        assert!(matches!(
            windows.check(at(2), &a, &root, &limits),
            Err(HubError::RateLimited(_))
        ));
        assert!(windows.check(at(2), &b, &root, &limits).is_ok());
        windows.admit(at(2), &b, &root, &limits).unwrap();
        assert!(matches!(
            windows.check(at(3), &b, &root, &limits),
            Err(HubError::RateLimited(_))
        ));
        assert_eq!(windows.sender_slots(&a), 2);
    }

    #[test]
    fn check_counts_only_the_live_window_with_a_strict_boundary() {
        let limits = HubLimits {
            per_sender_per_10m: 1,
            ..HubLimits::default()
        };
        let (a, root) = (id("a"), RootId::mcp(uuid::Uuid::nil()));
        let mut windows = Windows::default();
        windows.admit(at(0), &a, &root, &limits).unwrap();
        assert!(windows.check(at(599), &a, &root, &limits).is_err());
        assert!(windows.check(at(600), &a, &root, &limits).is_ok());
        assert_eq!(windows.sender_slots(&a), 1);
    }

    #[test]
    fn the_defaults_are_the_documented_limits() {
        let limits = HubLimits::default();
        assert_eq!(
            (
                limits.body_max,
                limits.subject_max,
                limits.inbox_max,
                limits.fanout_max,
                limits.thread_depth_max
            ),
            (8192, 120, 256, 20, 32)
        );
    }

    #[test]
    fn the_window_is_half_open_and_reports_the_wait() {
        let root = RootId::mcp(uuid::Uuid::nil());
        let limits = HubLimits {
            per_sender_per_10m: 2,
            ..HubLimits::default()
        };
        let mut windows = Windows::default();
        windows.admit(at(0), &id("a"), &root, &limits).unwrap();
        windows.admit(at(100), &id("a"), &root, &limits).unwrap();
        let err = windows
            .admit(at(599), &id("a"), &root, &limits)
            .unwrap_err();
        assert!(
            matches!(err, HubError::RateLimited(wait) if wait == std::time::Duration::from_secs(1)),
            "{err:?}"
        );
        windows.admit(at(600), &id("a"), &root, &limits).unwrap();
        windows.admit(at(0), &id("b"), &root, &limits).unwrap();
    }

    #[test]
    fn expired_entries_are_dropped_so_the_maps_stay_bounded() {
        let limits = HubLimits::default();
        let mut windows = Windows::default();
        for n in 0..50 {
            let root = RootId::mcp(uuid::Uuid::from_u128(n));
            windows
                .admit(at(0), &id(&format!("s{n}")), &root, &limits)
                .unwrap();
        }
        assert_eq!(windows.tracked(), (50, 50));
        let root = RootId::mcp(uuid::Uuid::from_u128(1000));
        windows.admit(at(600), &id("late"), &root, &limits).unwrap();
        assert_eq!(windows.tracked(), (1, 51));
        windows
            .admit(at(3600), &id("later"), &root, &limits)
            .unwrap();
        assert_eq!(windows.tracked(), (1, 1));
        assert_eq!(windows.sender_slots(&id("later")), 1);
        assert_eq!(windows.sender_slots(&id("s0")), 0);
    }

    #[test]
    fn reservations_count_and_release_to_zero() {
        let mut windows = Windows::default();
        windows.reserve(&[id("a"), id("a"), id("b")]);
        assert_eq!(
            (
                windows.held(&id("a")),
                windows.held(&id("b")),
                windows.held(&id("c"))
            ),
            (2, 1, 0)
        );
        windows.release(&[id("a"), id("c")]);
        assert_eq!(windows.held(&id("a")), 1);
        windows.release(&[id("a"), id("b")]);
        assert_eq!((windows.held(&id("a")), windows.held(&id("b"))), (0, 0));
    }
}
