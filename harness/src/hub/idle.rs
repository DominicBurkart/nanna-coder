use super::lineage::SessionId;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The most sessions whose idle time the hub tracks at once; past it the least recently charged one is dropped.
pub(super) const IDLE_SESSIONS_MAX: usize = 4096;

#[derive(Debug, Default)]
struct Ledger {
    spent: HashMap<SessionId, (i64, u64)>,
    stamp: u64,
}

impl Ledger {
    fn oldest(&self) -> Option<SessionId> {
        let found = self.spent.iter().min_by_key(|(_, entry)| entry.1);
        found.map(|(key, _)| key.clone())
    }

    fn make_room(&mut self, session: &SessionId) {
        let full = self.spent.len() >= IDLE_SESSIONS_MAX && !self.spent.contains_key(session);
        let victim = if full { self.oldest() } else { None };
        if let Some(key) = victim {
            self.spent.remove(&key);
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct IdleBudget {
    ledger: Mutex<Ledger>,
}

impl IdleBudget {
    fn lock(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn charge(&self, session: &SessionId, millis: i64, cap_millis: i64) -> bool {
        let mut ledger = self.lock();
        let used = ledger.spent.get(session).map_or(0, |entry| entry.0);
        let total = used.saturating_add(millis);
        let fits = total <= cap_millis;
        if fits {
            ledger.make_room(session);
            ledger.stamp = ledger.stamp.saturating_add(1);
            let stamp = ledger.stamp;
            ledger.spent.insert(session.clone(), (total, stamp));
        }
        fits
    }

    pub(super) fn exhausted(&self, session: &SessionId, cap_millis: i64) -> bool {
        let ledger = self.lock();
        let used = ledger.spent.get(session).map_or(0, |entry| entry.0);
        used >= cap_millis
    }

    #[cfg(test)]
    pub(super) fn used(&self, session: &SessionId) -> i64 {
        self.lock().spent.get(session).map_or(0, |entry| entry.0)
    }

    #[cfg(test)]
    pub(super) fn tracked(&self) -> usize {
        self.lock().spent.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskId;

    fn id(name: &str) -> SessionId {
        SessionId::from_task(TaskId(name.into()))
    }

    #[test]
    fn a_charge_fits_only_while_the_total_stays_within_the_cap() {
        let budget = IdleBudget::default();
        let who = id("a");
        assert!(budget.charge(&who, 1000, 3000));
        assert!(budget.charge(&who, 2000, 3000));
        assert!(!budget.charge(&who, 1, 3000));
        assert_eq!(budget.used(&who), 3000);
        assert!(budget.exhausted(&who, 3000));
        assert!(!budget.exhausted(&who, 3001));
        assert!(!budget.exhausted(&id("other"), 3000));
        assert_eq!(budget.used(&id("other")), 0);
    }

    #[test]
    fn a_refused_charge_changes_nothing_and_a_zero_cap_is_always_spent() {
        let budget = IdleBudget::default();
        assert!(!budget.charge(&id("a"), 1000, 0));
        assert_eq!(budget.tracked(), 0);
        assert!(budget.exhausted(&id("a"), 0));
    }

    fn filled(budget: &IdleBudget) {
        for n in 0..4096 {
            assert!(budget.charge(&id(&format!("s{n}")), 1000, 5000));
        }
        assert_eq!(budget.tracked(), 4096);
    }

    #[test]
    fn a_full_map_still_gives_a_new_session_a_budget_by_dropping_the_least_recently_charged() {
        let budget = IdleBudget::default();
        filled(&budget);
        let late = id("late");
        assert!(!budget.exhausted(&late, 5000));
        assert!(budget.charge(&late, 1000, 5000));
        assert_eq!(budget.used(&late), 1000);
        assert_eq!(budget.tracked(), 4096);
        assert_eq!(budget.used(&id("s0")), 0);
        assert_eq!(budget.used(&id("s1")), 1000);
    }

    #[test]
    fn a_recently_charged_near_cap_session_is_not_the_one_dropped() {
        let budget = IdleBudget::default();
        filled(&budget);
        let busy = id("s0");
        for _ in 0..4 {
            assert!(budget.charge(&busy, 1000, 5000));
        }
        assert!(budget.charge(&id("late"), 1000, 5000));
        assert_eq!(budget.used(&busy), 5000);
        assert!(budget.exhausted(&busy, 5000));
        assert_eq!(budget.used(&id("s1")), 0);
        assert_eq!(budget.used(&id("s2")), 1000);
    }

    #[test]
    fn the_map_never_grows_past_its_bound_and_charging_a_known_session_drops_nobody() {
        let budget = IdleBudget::default();
        filled(&budget);
        assert!(budget.charge(&id("s7"), 1000, 5000));
        assert_eq!(budget.used(&id("s0")), 1000);
        assert_eq!(budget.tracked(), 4096);
        for n in 0..100 {
            assert!(budget.charge(&id(&format!("new{n}")), 1000, 5000));
        }
        assert_eq!(budget.tracked(), 4096);
        assert_eq!(budget.used(&id("s7")), 2000);
        assert_eq!(budget.used(&id("s0")), 0);
        assert_eq!(budget.used(&id("new99")), 1000);
    }

    #[test]
    fn a_poisoned_map_still_charges_and_still_stops_at_the_cap() {
        let budget = IdleBudget::default();
        let who = id("a");
        let held = std::panic::catch_unwind(|| {
            let _guard = budget.ledger.lock().unwrap();
            panic!("poison");
        });
        assert!(held.is_err());
        assert!(budget.charge(&who, 1000, 1000));
        assert!(!budget.charge(&who, 1000, 1000));
    }

    #[test]
    fn the_budget_never_touches_a_session_record() {
        let source = include_str!("idle.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for banned in [
            "registry",
            "finish",
            "cancel",
            "abandon",
            "fail",
            "set_status",
            ".unwrap(",
            ".expect(",
            "panic!",
        ] {
            assert!(!production.contains(banned), "{banned}");
        }
    }
}
