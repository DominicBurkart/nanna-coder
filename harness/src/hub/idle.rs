use super::lineage::SessionId;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The most sessions whose idle time the hub tracks at once; a session beyond it gets no wait.
pub(super) const IDLE_SESSIONS_MAX: usize = 4096;

#[derive(Debug, Default)]
pub(super) struct IdleBudget {
    spent: Mutex<HashMap<SessionId, i64>>,
}

impl IdleBudget {
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, i64>> {
        self.spent.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn charge(&self, session: &SessionId, millis: i64, cap_millis: i64) -> bool {
        let mut spent = self.lock();
        let room = spent.contains_key(session) || spent.len() < IDLE_SESSIONS_MAX;
        let used = spent.get(session).copied().unwrap_or(0);
        let fits = room && used.saturating_add(millis) <= cap_millis;
        if fits {
            spent.insert(session.clone(), used.saturating_add(millis));
        }
        fits
    }

    pub(super) fn exhausted(&self, session: &SessionId, cap_millis: i64) -> bool {
        let spent = self.lock();
        let used = spent.get(session).copied();
        let room = used.is_some() || spent.len() < IDLE_SESSIONS_MAX;
        !room || used.unwrap_or(0) >= cap_millis
    }

    #[cfg(test)]
    pub(super) fn used(&self, session: &SessionId) -> i64 {
        self.lock().get(session).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn tracked(&self) -> usize {
        self.lock().len()
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

    #[test]
    fn a_full_map_gives_a_new_session_no_wait_and_keeps_serving_known_ones() {
        let budget = IdleBudget::default();
        for n in 0..4096 {
            assert!(budget.charge(&id(&format!("s{n}")), 1000, 5000));
        }
        assert_eq!(budget.tracked(), 4096);
        let late = id("late");
        assert!(!budget.charge(&late, 1000, 5000));
        assert!(budget.exhausted(&late, 5000));
        assert_eq!(budget.tracked(), 4096);
        let known = id("s0");
        assert!(budget.charge(&known, 1000, 5000));
        assert!(!budget.exhausted(&known, 5000));
    }

    #[test]
    fn a_poisoned_map_still_charges_and_still_stops_at_the_cap() {
        let budget = IdleBudget::default();
        let who = id("a");
        let held = std::panic::catch_unwind(|| {
            let _guard = budget.spent.lock().unwrap();
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
