use super::lineage::SessionId;
use super::message::MessageId;
use crate::auditor::ReasonCode;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The most notices the hub keeps for one session; the oldest is dropped and counted.
pub const NOTICES_PER_SESSION: usize = 16;

/// A hub-authored record that a message the session sent was withheld by the recipient's
/// inbound review. It carries the message id and the fixed reason codes, never a subject, a
/// body or any text a session wrote. It is not a `StoredMessage`, is never fenced as agent
/// text and is built in exactly one place, the inbound `Block` path of `recv`.
///
/// ```
/// use harness::hub::notice::SenderNotice;
///
/// let notice = serde_json::to_value(SenderNotice::example()).unwrap();
/// let mut keys: Vec<&str> = notice.as_object().unwrap().keys().map(String::as_str).collect();
/// keys.sort();
/// assert_eq!(keys, ["at", "codes", "message_id", "rendered"]);
/// assert!(notice["rendered"].as_str().unwrap().starts_with("[nanna] message "));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SenderNotice {
    pub message_id: MessageId,
    pub codes: Vec<ReasonCode>,
    pub at: DateTime<Utc>,
    rendered: String,
}

impl SenderNotice {
    /// The host-authored line the model reads: the message id and the reason code names.
    ///
    /// ```
    /// use harness::hub::notice::SenderNotice;
    ///
    /// assert!(SenderNotice::example().rendered().ends_with("was not delivered: secret_leak"));
    /// ```
    pub fn rendered(&self) -> &str {
        &self.rendered
    }

    pub(super) fn new(message_id: MessageId, codes: &[ReasonCode], at: DateTime<Utc>) -> Self {
        let codes = if codes.is_empty() {
            vec![ReasonCode::Other]
        } else {
            codes.to_vec()
        };
        let names: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        let id = message_id.as_uuid();
        let rendered = format!(
            "[nanna] message {id} was not delivered: {}",
            names.join(", ")
        );
        Self {
            message_id,
            codes,
            at,
            rendered,
        }
    }

    /// A fixed notice for documentation.
    #[doc(hidden)]
    pub fn example() -> Self {
        let id = MessageId::from_uuid(uuid::Uuid::nil());
        Self::new(id, &[ReasonCode::SecretLeak], DateTime::<Utc>::UNIX_EPOCH)
    }
}

/// The notices one `recv` or `send` result carries: oldest first, plus how many older ones were
/// dropped because the session had more than [`NOTICES_PER_SESSION`] waiting.
///
/// ```
/// use harness::hub::notice::Notices;
///
/// let none = Notices::default();
/// assert!(none.is_empty());
/// assert_eq!(serde_json::to_string(&none).unwrap(), r#"{"items":[],"dropped":0}"#);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Notices {
    pub items: Vec<SenderNotice>,
    pub dropped: usize,
}

impl Notices {
    /// True when there is nothing to report.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.dropped == 0
    }
}

#[derive(Default)]
struct Queue {
    items: VecDeque<SenderNotice>,
    dropped: usize,
}

#[derive(Default)]
pub(super) struct NoticeTable {
    queues: Mutex<HashMap<SessionId, Queue>>,
}

impl NoticeTable {
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Queue>> {
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn push(&self, session: &SessionId, notice: SenderNotice) {
        let mut queues = self.lock();
        let queue = queues.entry(session.clone()).or_default();
        if queue.items.len() >= NOTICES_PER_SESSION {
            queue.items.pop_front();
            queue.dropped = queue.dropped.saturating_add(1);
        }
        queue.items.push_back(notice);
    }

    pub(super) fn take(&self, session: &SessionId) -> Notices {
        match self.lock().remove(session) {
            Some(queue) => Notices {
                items: Vec::from(queue.items),
                dropped: queue.dropped,
            },
            None => Notices::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn poison(&self) {
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.poisoning()));
        assert!(attempt.is_err());
    }

    #[cfg(test)]
    fn poisoning(&self) {
        let _held = self.queues.lock();
        panic!("poisoned on purpose");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskId;
    use chrono::TimeZone;
    use proptest::prelude::*;
    use std::sync::Arc;

    fn at() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    fn session(name: &str) -> SessionId {
        SessionId::from_task(TaskId(name.to_string()))
    }

    fn notice(n: u128) -> SenderNotice {
        let id = MessageId::from_uuid(uuid::Uuid::from_u128(n));
        SenderNotice::new(id, &[ReasonCode::PromptInjection], at())
    }

    fn ids(notices: &Notices) -> Vec<u128> {
        let mut out = Vec::new();
        for item in &notices.items {
            out.push(item.message_id.as_uuid().as_u128());
        }
        out
    }

    #[test]
    fn a_notice_is_the_id_the_codes_and_a_fixed_sentence_and_nothing_else() {
        let id = MessageId::from_uuid(uuid::Uuid::from_u128(7));
        let made = SenderNotice::new(id, &[ReasonCode::SecretLeak, ReasonCode::Other], at());
        assert_eq!(made.message_id, id);
        assert_eq!(made.codes, [ReasonCode::SecretLeak, ReasonCode::Other]);
        assert_eq!(made.at, at());
        let line = "[nanna] message 00000000-0000-0000-0000-000000000007 was not delivered: secret_leak, other";
        assert_eq!(made.rendered(), line);
        let json = serde_json::to_value(&made).unwrap();
        assert_eq!(json["rendered"], line);
        assert_eq!(json["codes"], serde_json::json!(["secret_leak", "other"]));
    }

    #[test]
    fn a_block_with_no_reason_code_still_names_one() {
        let made = SenderNotice::new(MessageId::new(), &[], at());
        assert_eq!(made.codes, [ReasonCode::Other]);
        assert!(made.rendered().ends_with("was not delivered: other"));
    }

    #[test]
    fn the_documentation_example_is_a_secret_leak_notice_for_the_nil_id() {
        let example = SenderNotice::example();
        assert_eq!(example.codes, [ReasonCode::SecretLeak]);
        assert!(example.message_id.as_uuid().is_nil());
    }

    #[test]
    fn notices_are_taken_once_oldest_first_and_only_by_their_session() {
        let table = NoticeTable::default();
        let (a, b) = (session("a"), session("b"));
        for n in 1..=3 {
            table.push(&a, notice(n));
        }
        table.push(&b, notice(9));
        assert!(table.take(&session("c")).is_empty());
        let first = table.take(&a);
        assert_eq!((ids(&first), first.dropped), (vec![1, 2, 3], 0));
        assert!(table.take(&a).is_empty());
        assert_eq!(ids(&table.take(&b)), [9]);
    }

    #[test]
    fn a_session_keeps_the_newest_sixteen_and_counts_the_dropped_ones() {
        let table = NoticeTable::default();
        let a = session("a");
        for n in 1..=(NOTICES_PER_SESSION as u128 + 3) {
            table.push(&a, notice(n));
        }
        let taken = table.take(&a);
        assert_eq!(taken.items.len(), NOTICES_PER_SESSION);
        assert_eq!(taken.dropped, 3);
        assert_eq!(ids(&taken).first(), Some(&4));
        assert_eq!(ids(&taken).last(), Some(&19));
        assert!(!taken.is_empty());
        let fresh = table.take(&a);
        assert_eq!((fresh.items.len(), fresh.dropped), (0, 0));
    }

    #[test]
    fn a_count_alone_is_not_empty() {
        let only_dropped = Notices {
            items: Vec::new(),
            dropped: 2,
        };
        assert!(!only_dropped.is_empty());
        let only_items = Notices {
            items: vec![notice(1)],
            dropped: 0,
        };
        assert!(!only_items.is_empty());
    }

    #[test]
    fn empty_notices_leave_the_wire_shape_unchanged_and_full_ones_show() {
        #[derive(Serialize)]
        struct Page {
            seen: u8,
            #[serde(skip_serializing_if = "Notices::is_empty")]
            notices: Notices,
        }
        let bare_page = Page {
            seen: 1,
            notices: Notices::default(),
        };
        let bare = serde_json::to_value(bare_page).unwrap();
        assert_eq!(bare, serde_json::json!({"seen": 1}));
        let held = Notices {
            items: vec![notice(5)],
            dropped: 1,
        };
        let full_page = Page {
            seen: 1,
            notices: held,
        };
        let full = serde_json::to_value(full_page).unwrap();
        assert_eq!(full["notices"]["dropped"], 1);
        assert_eq!(full["notices"]["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_poisoned_table_still_stores_and_hands_over_notices() {
        let table = NoticeTable::default();
        table.poison();
        let a = session("a");
        table.push(&a, notice(1));
        assert_eq!(ids(&table.take(&a)), [1]);
    }

    #[test]
    fn concurrent_pushes_are_all_kept_up_to_the_cap() {
        let table = Arc::new(NoticeTable::default());
        let a = session("a");
        let mut workers = Vec::new();
        for worker in 0..4u128 {
            let (table, a) = (table.clone(), a.clone());
            workers.push(std::thread::spawn(move || table.push(&a, notice(worker))));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let mut seen = ids(&table.take(&a));
        seen.sort();
        assert_eq!(seen, [0, 1, 2, 3]);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Push(u8),
        Take(u8),
    }

    fn plans() -> impl Strategy<Value = Vec<Op>> {
        prop::collection::vec(
            prop_oneof![(0u8..3).prop_map(Op::Push), (0u8..3).prop_map(Op::Take)],
            1..120,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn the_table_matches_a_plain_list_model_with_a_sixteen_cap(plan in plans()) {
            let table = NoticeTable::default();
            let names = ["s0", "s1", "s2"];
            let mut model: Vec<Vec<u128>> = vec![Vec::new(); 3];
            let mut lost: Vec<usize> = vec![0; 3];
            let mut next = 0u128;
            for op in plan {
                match op {
                    Op::Push(who) => {
                        next += 1;
                        let who = usize::from(who);
                        table.push(&session(names[who]), notice(next));
                        model[who].push(next);
                        if model[who].len() > 16 {
                            model[who].remove(0);
                            lost[who] += 1;
                        }
                    }
                    Op::Take(who) => {
                        let who = usize::from(who);
                        let got = table.take(&session(names[who]));
                        prop_assert_eq!(ids(&got), model[who].clone());
                        prop_assert_eq!(got.dropped, lost[who]);
                        model[who].clear();
                        lost[who] = 0;
                    }
                }
            }
        }
    }
}
