use super::lineage::SessionId;
use super::registry::RegistryError;
use super::send::MessageHub;
use super::session::SessionStatus;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Waiter {
    count: usize,
    entered: bool,
}

#[derive(Debug, Default)]
pub(super) struct Waiters {
    by_session: Mutex<HashMap<SessionId, Waiter>>,
}

impl Waiters {
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Waiter>> {
        self.by_session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn is_waiting(&self, id: &SessionId) -> bool {
        self.lock().get(id).is_some_and(|waiter| waiter.count > 0)
    }
}

/// Keeps a session `WaitingOnMessages` while any blocking `recv` of that session is alive.
/// Dropping the guard (return, error or a cancelled future) releases one waiter.
pub(super) struct WaitGuard<'a> {
    hub: &'a MessageHub,
    session: SessionId,
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.hub.leave_wait(&self.session);
    }
}

impl MessageHub {
    pub(super) fn enter_wait(&self, session: &SessionId) -> WaitGuard<'_> {
        let mut waiters = self.waiters.lock();
        let waiter = waiters.entry(session.clone()).or_default();
        if waiter.count == 0 {
            let entered = self.registry.set_status(
                session,
                SessionStatus::WaitingOnMessages,
                self.clock.now(),
            );
            waiter.entered = self.settled(entered);
        }
        waiter.count += 1;
        WaitGuard {
            hub: self,
            session: session.clone(),
        }
    }

    fn leave_wait(&self, session: &SessionId) {
        let mut waiters = self.waiters.lock();
        let waiter = waiters.entry(session.clone()).or_default();
        waiter.count = waiter.count.saturating_sub(1);
        if waiter.count == 0 {
            let entered = waiter.entered;
            waiters.remove(session);
            if entered {
                let left = self.registry.leave_waiting(session, self.clock.now());
                self.settled(left);
            }
        }
    }

    fn settled<T>(&self, moved: Result<T, RegistryError>) -> bool {
        if moved.is_err() {
            tracing::debug!("the hub could not move a session between running and waiting");
        }
        moved.is_ok()
    }
}

#[cfg(test)]
mod tests {
    fn production_source() -> String {
        let source = include_str!("waiting.rs");
        source.split("#[cfg(test)]").next().unwrap().to_string()
    }

    #[test]
    fn production_code_has_no_panic_paths_no_indexing_and_no_text_in_logs() {
        let source = production_source();
        for banned in [
            ".unwrap(",
            ".expect(",
            "panic!",
            "unreachable!",
            "todo!",
            "unimplemented!",
        ] {
            assert!(!source.contains(banned), "{banned}");
        }
        let chars: Vec<char> = source.chars().collect();
        for pair in chars.windows(2) {
            let indexing =
                (pair[0].is_alphanumeric() || pair[0] == '_' || pair[0] == ')') && pair[1] == '[';
            assert!(!indexing, "indexing near {pair:?}");
        }
        let logs: Vec<&str> = source
            .match_indices("tracing::")
            .map(|(at, _)| &source[at..])
            .collect();
        assert_eq!(logs.len(), 1);
        for log in logs {
            let call = log.split(");").next().unwrap();
            assert!(
                call.contains("\"the hub could not move a session between running and waiting\""),
                "{call}"
            );
            assert!(!call.contains('{') && !call.contains(','), "{call}");
        }
    }

    #[test]
    fn the_hub_enters_with_set_status_and_leaves_only_through_leave_waiting() {
        let source = production_source();
        let variants: Vec<&str> = source
            .split("SessionStatus::")
            .skip(1)
            .filter_map(|rest| rest.split(|c: char| !c.is_alphanumeric()).next())
            .collect();
        assert_eq!(variants, ["WaitingOnMessages"]);
        assert_eq!(source.matches("self.registry.").count(), 2);
        assert_eq!(source.matches("self.registry.set_status(").count(), 1);
        assert_eq!(source.matches("self.registry.leave_waiting(").count(), 1);
        for banned in [".finish(", ".cancel(", ".abandon(", ".sessions", "set_note"] {
            assert!(!source.contains(banned), "{banned}");
        }
    }
}
