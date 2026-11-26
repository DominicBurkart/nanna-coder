use crate::hub::doorbell::Doorbell;
use crate::hub::lineage::SessionId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::watch;

struct Slot {
    generation: u64,
    bell: watch::Sender<usize>,
}

/// The doorbell for pi sessions. It maps a running session to a one-slot channel that carries
/// only the unread count, so no text an agent wrote can reach the executor through it. A session
/// with no registered channel (every `AgentLoop` session, and a pi session that has ended) is
/// rung to no effect. The executor owns the only writer of the steer line and the channel entry
/// is removed on every exit of the run.
///
/// ```
/// use harness::harness_adapter::PiSteerDoorbell;
/// use harness::hub::doorbell::Doorbell;
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let bell = PiSteerDoorbell::new();
/// let nobody = SessionId::from_task(TaskId("nobody".to_string()));
/// Doorbell::ring(&bell, &nobody, 2);
/// assert!(!bell.is_registered(&nobody));
/// ```
#[derive(Default)]
pub struct PiSteerDoorbell {
    slots: Mutex<HashMap<SessionId, Slot>>,
    issued: AtomicU64,
}

impl std::fmt::Debug for PiSteerDoorbell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiSteerDoorbell").finish_non_exhaustive()
    }
}

impl PiSteerDoorbell {
    /// A doorbell with no session registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a running pi session is currently registered under `session`.
    ///
    /// ```
    /// use harness::harness_adapter::PiSteerDoorbell;
    /// use harness::hub::lineage::SessionId;
    /// use harness::task::TaskId;
    ///
    /// let bell = PiSteerDoorbell::new();
    /// assert!(!bell.is_registered(&SessionId::from_task(TaskId("a".to_string()))));
    /// ```
    pub fn is_registered(&self, session: &SessionId) -> bool {
        self.lock().contains_key(session)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn register(self: &Arc<Self>, session: SessionId) -> SteerHandle {
        let generation = self.issued.fetch_add(1, Ordering::SeqCst);
        let (bell, inbox) = watch::channel(0usize);
        self.lock()
            .insert(session.clone(), Slot { generation, bell });
        let owner = Arc::clone(self);
        SteerHandle {
            inbox,
            _guard: SteerGuard {
                owner,
                session,
                generation,
            },
        }
    }
}

impl Doorbell for PiSteerDoorbell {
    fn ring(&self, to: &SessionId, unread: usize) {
        if let Some(slot) = self.lock().get(to) {
            slot.bell.send_replace(unread);
        }
    }
}

struct SteerGuard {
    owner: Arc<PiSteerDoorbell>,
    session: SessionId,
    generation: u64,
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        let mut slots = self.owner.lock();
        let ours = slots.get(&self.session).map(|slot| slot.generation);
        if ours == Some(self.generation) {
            slots.remove(&self.session);
        }
    }
}

pub(crate) struct SteerHandle {
    inbox: watch::Receiver<usize>,
    _guard: SteerGuard,
}

impl SteerHandle {
    pub(crate) async fn next(&mut self) -> usize {
        if self.inbox.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        *self.inbox.borrow_and_update()
    }
}

#[derive(Debug, Default)]
pub(super) struct Coalescer {
    armed: bool,
    rung: bool,
    dirty: Option<usize>,
}

impl Coalescer {
    pub(super) fn on_ring(&mut self, count: usize) -> Option<usize> {
        let now = self.armed && !self.rung;
        self.rung = self.rung || now;
        self.dirty = (!now).then_some(count);
        now.then_some(count)
    }

    pub(super) fn arm(&mut self) -> Option<usize> {
        self.armed = true;
        self.flush()
    }

    pub(super) fn turn_start(&mut self) -> Option<usize> {
        self.rung = false;
        self.flush()
    }

    fn flush(&mut self) -> Option<usize> {
        let due = self.dirty.filter(|_| self.armed);
        self.dirty = self.dirty.filter(|_| !self.armed);
        self.rung = self.rung || due.is_some();
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskId;
    use proptest::prelude::*;
    use std::time::Duration;

    fn sid(name: &str) -> SessionId {
        SessionId::from_task(TaskId(name.to_string()))
    }

    fn bell() -> Arc<PiSteerDoorbell> {
        Arc::new(PiSteerDoorbell::new())
    }

    fn ring(bell: &PiSteerDoorbell, to: &SessionId, count: usize) {
        Doorbell::ring(bell, to, count);
    }

    async fn quiet(handle: &mut SteerHandle) -> bool {
        let wait = Duration::from_millis(60);
        tokio::time::timeout(wait, handle.next()).await.is_err()
    }

    #[tokio::test]
    async fn a_ring_reaches_exactly_the_registered_recipient_with_the_count() {
        let bell = bell();
        let mut a = bell.register(sid("a"));
        let mut b = bell.register(sid("b"));
        ring(&bell, &sid("a"), 3);
        assert_eq!(a.next().await, 3);
        assert!(quiet(&mut b).await);
    }

    #[tokio::test]
    async fn a_ring_for_a_session_nobody_registered_is_a_no_op() {
        let bell = bell();
        let mut a = bell.register(sid("a"));
        ring(&bell, &sid("stranger"), 9);
        assert!(quiet(&mut a).await);
        assert!(!bell.is_registered(&sid("stranger")));
    }

    #[tokio::test]
    async fn rings_before_a_read_keep_only_the_latest_count_and_none_is_lost() {
        let bell = bell();
        let mut a = bell.register(sid("a"));
        for n in [1, 2, 7] {
            ring(&bell, &sid("a"), n);
        }
        assert_eq!(a.next().await, 7);
        assert!(quiet(&mut a).await);
    }

    #[tokio::test]
    async fn the_entry_is_removed_when_the_handle_is_dropped() {
        let bell = bell();
        let handle = bell.register(sid("a"));
        assert!(bell.is_registered(&sid("a")));
        drop(handle);
        assert!(!bell.is_registered(&sid("a")));
        ring(&bell, &sid("a"), 1);
    }

    #[tokio::test]
    async fn an_older_handle_never_removes_a_newer_entry_and_goes_quiet() {
        let bell = bell();
        let mut older = bell.register(sid("a"));
        let mut newer = bell.register(sid("a"));
        assert!(quiet(&mut older).await);
        drop(older);
        assert!(bell.is_registered(&sid("a")));
        ring(&bell, &sid("a"), 4);
        assert_eq!(newer.next().await, 4);
    }

    #[test]
    fn the_debug_form_names_no_session() {
        let bell = bell();
        let _handle = bell.register(sid("secret-session"));
        let shown = format!("{:?}", bell.as_ref());
        assert_eq!(shown, "PiSteerDoorbell { .. }");
    }

    #[test]
    fn a_poisoned_lock_still_serves_rings_and_lookups() {
        let bell = bell();
        let held = Arc::clone(&bell);
        let crashed = std::thread::spawn(move || {
            let _slots = held.slots.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(crashed.is_err());
        assert!(!bell.is_registered(&sid("a")));
        ring(&bell, &sid("a"), 1);
    }

    #[tokio::test]
    async fn a_send_succeeds_the_same_whether_or_not_the_recipient_has_a_registered_run() {
        use crate::hub::runtime::tests::{assembled_with, mail, seat};
        let bell = bell();
        let hub = assembled_with(bell.clone());
        let (sender, idle, running) = (seat(&hub, "s"), seat(&hub, "idle"), seat(&hub, "run"));
        let mut inbox = bell.register(running.session().clone());
        let to_idle = hub.hub().unwrap().send(&sender, mail(&idle)).await.unwrap();
        let to_running = hub
            .hub()
            .unwrap()
            .send(&sender, mail(&running))
            .await
            .unwrap();
        assert_eq!(
            (to_idle.recipients, to_idle.notices),
            (1, Default::default())
        );
        assert_eq!(
            (to_running.recipients, to_running.notices),
            (1, Default::default())
        );
        assert_eq!(inbox.next().await, 1);
    }

    #[test]
    fn nothing_is_written_before_the_broker_is_armed_and_the_latest_count_is_held() {
        let mut c = Coalescer::default();
        assert_eq!(c.on_ring(1), None);
        assert_eq!(c.turn_start(), None);
        assert_eq!(c.on_ring(4), None);
        assert_eq!(c.arm(), Some(4));
        assert_eq!(c.turn_start(), None);
    }

    #[test]
    fn the_first_ring_of_a_turn_writes_and_later_rings_wait_for_the_next_turn() {
        let mut c = Coalescer::default();
        assert_eq!(c.arm(), None);
        assert_eq!(c.on_ring(1), Some(1));
        assert_eq!(c.on_ring(2), None);
        assert_eq!(c.on_ring(3), None);
        assert_eq!(c.turn_start(), Some(3));
        assert_eq!(c.on_ring(4), None);
        assert_eq!(c.turn_start(), Some(4));
        assert_eq!(c.turn_start(), None);
        assert_eq!(c.on_ring(5), Some(5));
    }

    #[test]
    fn a_ring_in_a_fresh_turn_after_a_quiet_one_writes_at_once() {
        let mut c = Coalescer::default();
        assert_eq!(c.arm(), None);
        assert_eq!(c.turn_start(), None);
        assert_eq!(c.on_ring(2), Some(2));
    }

    #[derive(Debug, Clone)]
    enum Event {
        Ring(usize),
        Turn,
    }

    fn events() -> impl Strategy<Value = Vec<Event>> {
        let one = prop_oneof![(1usize..50).prop_map(Event::Ring), Just(Event::Turn)];
        prop::collection::vec(one, 0..60)
    }

    proptest! {
        #[test]
        fn at_most_one_steer_per_turn_and_no_ring_is_lost(events in events()) {
            let mut c = Coalescer::default();
            c.arm();
            let mut writes_this_turn = 0usize;
            let mut unsent: Option<usize> = None;
            for event in &events {
                match event {
                    Event::Ring(n) => {
                        if let Some(sent) = c.on_ring(*n) {
                            writes_this_turn += 1;
                            prop_assert_eq!(sent, *n);
                        } else {
                            unsent = Some(*n);
                        }
                    }
                    Event::Turn => {
                        writes_this_turn = 0;
                        let due = c.turn_start();
                        prop_assert_eq!(due, unsent.take());
                        writes_this_turn += usize::from(due.is_some());
                    }
                }
                prop_assert!(writes_this_turn <= 1);
            }
            prop_assert_eq!(c.turn_start(), unsent.take());
        }
    }
}
