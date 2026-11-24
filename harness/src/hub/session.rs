use super::lineage::{Lineage, SessionId};
use super::summary::Summary;
use crate::effects::EffectClass;
use crate::identity::DevLoop;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Lifecycle state of a session.
///
/// ```
/// use harness::hub::session::SessionStatus;
///
/// assert!(SessionStatus::Running.is_active());
/// assert!(SessionStatus::WaitingOnMessages.is_active());
/// assert!(!SessionStatus::Completed.is_active());
/// assert_eq!(serde_json::to_string(&SessionStatus::WaitingOnMessages).unwrap(), "\"waiting_on_messages\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Running,
    WaitingOnMessages,
    Completed,
    Failed,
    Cancelled,
}

impl SessionStatus {
    /// The snake_case name used in storage and in summaries.
    ///
    /// ```
    /// use harness::hub::session::SessionStatus;
    ///
    /// assert_eq!(SessionStatus::WaitingOnMessages.label(), "waiting_on_messages");
    /// assert_eq!(SessionStatus::Running.label(), "running");
    /// ```
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Pending => "pending",
            SessionStatus::Running => "running",
            SessionStatus::WaitingOnMessages => "waiting_on_messages",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::Cancelled => "cancelled",
        }
    }

    /// True while the session has not reached a terminal status.
    ///
    /// ```
    /// use harness::hub::session::SessionStatus;
    ///
    /// assert!(SessionStatus::Pending.is_active());
    /// assert!(!SessionStatus::Failed.is_active());
    /// assert!(!SessionStatus::Cancelled.is_active());
    /// ```
    pub fn is_active(self) -> bool {
        matches!(
            self,
            SessionStatus::Pending | SessionStatus::Running | SessionStatus::WaitingOnMessages
        )
    }
}

/// What kind of session a record describes.
///
/// ```
/// use harness::hub::session::SessionKind;
///
/// assert_eq!(serde_json::to_string(&SessionKind::Librarian).unwrap(), "\"librarian\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Agent,
    Librarian,
}

/// One session as the hub stores it. `note` is a bounded [`Summary`]; a
/// stored note that is not a valid summary (a record written before the type
/// existed) loads as `None` rather than failing the replay.
///
/// ```
/// use chrono::Utc;
/// use harness::effects::EffectClass;
/// use harness::hub::lineage::{Lineage, RootId, SessionId};
/// use harness::hub::session::{ReadScope, SessionKind, SessionRecord, SessionStatus};
/// use harness::identity::DevLoop;
/// use harness::task::TaskId;
/// use uuid::Uuid;
///
/// let now = Utc::now();
/// let record = SessionRecord {
///     id: SessionId::from_task(TaskId("t-1".into())),
///     lineage: Lineage::root_child(RootId::mcp(Uuid::nil())),
///     kind: SessionKind::Agent,
///     identity: "coder".into(),
///     role: "writes code".into(),
///     dev_loop: DevLoop::Inner,
///     max_effect: EffectClass::Workspace,
///     tools: vec!["read_file".into()],
///     write_paths: vec!["src".into()],
///     read_paths: ReadScope::Unrestricted,
///     subtask: "fix the bug".into(),
///     note: None,
///     result_summary: None,
///     touched_paths: vec![],
///     status: SessionStatus::Pending,
///     created_at: now,
///     last_activity: now,
///     ended_at: None,
/// };
/// assert!(!format!("{record:?}").contains("fix the bug"));
/// let json = serde_json::to_string(&record).unwrap();
/// assert_eq!(serde_json::from_str::<SessionRecord>(&json).unwrap(), record);
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub lineage: Lineage,
    pub kind: SessionKind,
    pub identity: String,
    pub role: String,
    pub dev_loop: DevLoop,
    pub max_effect: EffectClass,
    pub tools: Vec<String>,
    pub write_paths: Vec<String>,
    #[serde(default, deserialize_with = "lenient_read_scope")]
    pub read_paths: ReadScope,
    pub subtask: String,
    #[serde(default, deserialize_with = "lenient_note")]
    pub note: Option<Summary>,
    pub result_summary: Option<String>,
    pub touched_paths: Vec<String>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// What a session may read, recorded when it is registered from its audited identity card.
///
/// `Unknown` is the explicit serde default: a record written before read paths were recorded, or
/// one whose stored value is unreadable, loads as `Unknown`. It is never read as `Unrestricted`.
/// The privilege comparison in section 8.2 fails closed in both directions: a sender whose scope
/// is `Unknown` has no read authority at all, and a recipient whose scope is `Unknown` is treated
/// as able to read everything. `Debug` prints the number of paths and never a path.
///
/// ```
/// use harness::hub::session::ReadScope;
///
/// assert_eq!(ReadScope::default(), ReadScope::Unknown);
/// let listed = ["docs/**".to_string()];
/// assert_eq!(ReadScope::of_card(None), ReadScope::Unrestricted);
/// assert_eq!(ReadScope::of_card(Some(&listed)), ReadScope::Paths(listed.to_vec()));
/// assert_eq!(ReadScope::Unknown.for_sender(), Some(Vec::new()));
/// assert_eq!(ReadScope::Unknown.for_recipient(), None);
/// assert_eq!(format!("{:?}", ReadScope::Paths(listed.to_vec())), "Paths(1)");
/// ```
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadScope {
    #[default]
    Unknown,
    Unrestricted,
    Paths(Vec<String>),
}

impl ReadScope {
    /// The scope an audited card declares: no `scope.read_paths` means unrestricted reads.
    ///
    /// ```
    /// use harness::hub::session::ReadScope;
    ///
    /// assert_eq!(ReadScope::of_card(None), ReadScope::Unrestricted);
    /// assert_eq!(ReadScope::of_card(Some(&[])), ReadScope::Paths(Vec::new()));
    /// ```
    pub fn of_card(read_paths: Option<&[String]>) -> Self {
        read_paths.map_or(ReadScope::Unrestricted, |paths| {
            ReadScope::Paths(paths.to_vec())
        })
    }

    /// The read set as the sender side of the privilege comparison: `None` covers any recipient,
    /// `Unknown` is the empty set (no authority).
    ///
    /// ```
    /// use harness::hub::session::ReadScope;
    ///
    /// assert_eq!(ReadScope::Unrestricted.for_sender(), None);
    /// assert_eq!(ReadScope::Unknown.for_sender(), Some(Vec::new()));
    /// ```
    pub fn for_sender(&self) -> Option<Vec<String>> {
        match self {
            ReadScope::Unknown => Some(Vec::new()),
            ReadScope::Unrestricted => None,
            ReadScope::Paths(paths) => Some(paths.clone()),
        }
    }

    /// The read set as the recipient side of the privilege comparison: `None` means it may read
    /// anything, which is how `Unknown` counts.
    ///
    /// ```
    /// use harness::hub::session::ReadScope;
    ///
    /// assert_eq!(ReadScope::Unknown.for_recipient(), None);
    /// assert_eq!(ReadScope::Paths(vec!["a".into()]).for_recipient(), Some(vec!["a".to_string()]));
    /// ```
    pub fn for_recipient(&self) -> Option<Vec<String>> {
        match self {
            ReadScope::Unknown | ReadScope::Unrestricted => None,
            ReadScope::Paths(paths) => Some(paths.clone()),
        }
    }
}

impl std::fmt::Debug for ReadScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadScope::Unknown => f.write_str("Unknown"),
            ReadScope::Unrestricted => f.write_str("Unrestricted"),
            ReadScope::Paths(paths) => write!(f, "Paths({})", paths.len()),
        }
    }
}

pub(crate) fn lenient_read_scope<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<ReadScope, D::Error> {
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(ReadScope::deserialize(raw).unwrap_or_default())
}

pub(crate) fn lenient_note<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Summary>, D::Error> {
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|text| Summary::try_from(text).ok()))
}

impl std::fmt::Debug for SessionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRecord")
            .field("id", &self.id)
            .field("lineage", &self.lineage)
            .field("kind", &self.kind)
            .field("dev_loop", &self.dev_loop)
            .field("max_effect", &self.max_effect)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("last_activity", &self.last_activity)
            .field("ended_at", &self.ended_at)
            .field("subtask_bytes", &self.subtask.len())
            .field(
                "note_bytes",
                &self.note.as_ref().map(|note| note.as_str().len()),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::lineage::RootId;
    use crate::task::TaskId;
    use proptest::prelude::*;

    fn record() -> SessionRecord {
        let now = Utc::now();
        SessionRecord {
            id: SessionId::from_task(TaskId("t".into())),
            lineage: Lineage::root_child(RootId::mcp(uuid::Uuid::nil())),
            kind: SessionKind::Agent,
            identity: "coder".into(),
            role: "role-SECRET".into(),
            dev_loop: DevLoop::Inner,
            max_effect: EffectClass::Workspace,
            tools: vec!["tool-SECRET".into()],
            write_paths: vec!["path-SECRET".into()],
            read_paths: ReadScope::Paths(vec!["read-SECRET".into()]),
            subtask: "subtask-SECRET".into(),
            note: Some(Summary::compose("coder", "running", "note-SECRET")),
            result_summary: Some("result-SECRET".into()),
            touched_paths: vec!["touched-SECRET".into()],
            status: SessionStatus::Running,
            created_at: now,
            last_activity: now,
            ended_at: None,
        }
    }

    #[test]
    fn only_non_terminal_statuses_are_active() {
        use SessionStatus::*;
        for (status, active) in [
            (Pending, true),
            (Running, true),
            (WaitingOnMessages, true),
            (Completed, false),
            (Failed, false),
            (Cancelled, false),
        ] {
            assert_eq!(status.is_active(), active, "{status:?}");
        }
    }

    #[test]
    fn labels_equal_the_serde_names() {
        use SessionStatus::*;
        for status in [
            Pending,
            Running,
            WaitingOnMessages,
            Completed,
            Failed,
            Cancelled,
        ] {
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{}\"", status.label())
            );
        }
    }

    #[test]
    fn a_stored_note_that_is_not_a_valid_summary_loads_as_none() {
        let mut value = serde_json::to_value(record()).unwrap();
        value["note"] = serde_json::json!("api_key=hunter2hunter2hunter2   spaced");
        let loaded: SessionRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(loaded.note, None);
        assert!(!format!("{loaded:?}").contains("hunter2"));
        value["note"] = serde_json::json!("x".repeat(500));
        assert_eq!(
            serde_json::from_value::<SessionRecord>(value.clone())
                .unwrap()
                .note,
            None
        );
        value.as_object_mut().unwrap().remove("note");
        assert_eq!(
            serde_json::from_value::<SessionRecord>(value.clone())
                .unwrap()
                .note,
            None
        );
        value["note"] = serde_json::json!("coder [running] fine");
        assert_eq!(
            serde_json::from_value::<SessionRecord>(value).unwrap().note,
            Some(Summary::compose("coder", "running", "fine"))
        );
    }

    #[test]
    fn debug_of_a_session_record_never_prints_agent_authored_text() {
        let debug = format!("{:?}", record());
        assert!(!debug.contains("SECRET"), "{debug}");
        assert!(debug.contains("subtask_bytes: 14"), "{debug}");
        assert!(debug.contains("note_bytes: Some(27)"), "{debug}");
    }

    #[test]
    fn a_record_round_trips_through_json_and_enums_use_snake_case() {
        let original = record();
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<SessionRecord>(&json).unwrap(),
            original
        );
        assert_eq!(
            serde_json::to_string(&SessionStatus::WaitingOnMessages).unwrap(),
            "\"waiting_on_messages\""
        );
        assert_eq!(
            serde_json::to_string(&SessionKind::Librarian).unwrap(),
            "\"librarian\""
        );
    }
    fn paths(list: &[&str]) -> ReadScope {
        ReadScope::Paths(list.iter().map(|path| path.to_string()).collect())
    }

    fn all_states() -> Vec<ReadScope> {
        vec![
            ReadScope::Unknown,
            ReadScope::Unrestricted,
            paths(&[]),
            paths(&["a"]),
            paths(&["a", "b"]),
            paths(&["b"]),
        ]
    }

    fn scope_with(read_paths: Option<Vec<String>>) -> crate::action_auditor::RecipientScope {
        crate::action_auditor::RecipientScope {
            session: SessionId::from_task(TaskId("x".into())),
            identity: "x".into(),
            max_effect: EffectClass::None,
            tools: Vec::new(),
            write_paths: Vec::new(),
            read_paths,
        }
    }

    fn asymmetric(sender: &ReadScope, recipient: &ReadScope) -> bool {
        crate::action_auditor::is_privilege_asymmetric(
            &scope_with(recipient.for_recipient()),
            &scope_with(sender.for_sender()),
        )
    }

    #[test]
    fn the_explicit_default_of_a_read_scope_is_unknown_never_unrestricted() {
        assert_eq!(ReadScope::default(), ReadScope::Unknown);
    }

    #[test]
    fn a_record_written_before_read_paths_existed_loads_as_unknown() {
        let mut legacy = serde_json::to_value(record()).unwrap();
        legacy.as_object_mut().unwrap().remove("read_paths");
        assert!(legacy.get("read_paths").is_none());
        let loaded: SessionRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(loaded.read_paths, ReadScope::Unknown);
        assert_eq!(loaded.identity, record().identity);
    }

    #[test]
    fn every_read_scope_round_trips_through_a_record_in_snake_case() {
        for scope in all_states() {
            let mut original = record();
            original.read_paths = scope;
            let json = serde_json::to_string(&original).unwrap();
            let back: SessionRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(back, original);
        }
        assert_eq!(
            serde_json::to_string(&ReadScope::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&ReadScope::Unrestricted).unwrap(),
            "\"unrestricted\""
        );
        assert_eq!(
            serde_json::to_string(&paths(&["a"])).unwrap(),
            "{\"paths\":[\"a\"]}"
        );
    }

    #[test]
    fn a_garbled_read_scope_loads_as_unknown_and_keeps_the_rest_of_the_record() {
        let garbled = [
            "1",
            "null",
            "\"everything\"",
            "\"Unrestricted\"",
            "[]",
            "{}",
            "{\"paths\":\"src\"}",
            "{\"paths\":[1]}",
            "{\"other\":[]}",
            "{\"paths\":[\"a\"],\"extra\":true}",
        ];
        for raw in garbled {
            let mut value = serde_json::to_value(record()).unwrap();
            value["read_paths"] = serde_json::from_str(raw).unwrap();
            let loaded: SessionRecord = serde_json::from_value(value).unwrap();
            assert_eq!(loaded.read_paths, ReadScope::Unknown, "{raw}");
            assert_eq!(loaded.identity, record().identity, "{raw}");
        }
    }

    #[test]
    fn debug_of_a_read_scope_prints_counts_and_never_a_path() {
        assert_eq!(format!("{:?}", ReadScope::Unknown), "Unknown");
        assert_eq!(format!("{:?}", ReadScope::Unrestricted), "Unrestricted");
        let shown = format!("{:?}", paths(&["read-SECRET", "other-SECRET"]));
        assert_eq!(shown, "Paths(2)");
    }

    #[test]
    fn a_card_without_read_paths_is_unrestricted_and_one_with_them_is_exactly_those() {
        assert_eq!(ReadScope::of_card(None), ReadScope::Unrestricted);
        assert_eq!(ReadScope::of_card(Some(&[])), paths(&[]));
        let listed = ["docs/**".to_string(), "src/**".to_string()];
        assert_eq!(
            ReadScope::of_card(Some(&listed)),
            paths(&["docs/**", "src/**"])
        );
    }

    #[test]
    fn a_sender_reads_unknown_as_no_authority_and_a_recipient_reads_it_as_unrestricted() {
        let own = |list: &[&str]| Some(list.iter().map(|p| p.to_string()).collect::<Vec<_>>());
        assert_eq!(ReadScope::Unknown.for_sender(), own(&[]));
        assert_eq!(ReadScope::Unrestricted.for_sender(), None);
        assert_eq!(paths(&["a", "b"]).for_sender(), own(&["a", "b"]));
        assert_eq!(ReadScope::Unknown.for_recipient(), None);
        assert_eq!(ReadScope::Unrestricted.for_recipient(), None);
        assert_eq!(paths(&["a", "b"]).for_recipient(), own(&["a", "b"]));
    }

    #[test]
    fn the_privilege_table_over_every_sender_and_recipient_state() {
        let rows: Vec<(ReadScope, Vec<(ReadScope, bool)>)> = vec![
            (
                ReadScope::Unknown,
                vec![
                    (ReadScope::Unknown, true),
                    (ReadScope::Unrestricted, true),
                    (paths(&[]), false),
                    (paths(&["a"]), true),
                    (paths(&["a", "b"]), true),
                ],
            ),
            (
                ReadScope::Unrestricted,
                vec![
                    (ReadScope::Unknown, false),
                    (ReadScope::Unrestricted, false),
                    (paths(&[]), false),
                    (paths(&["a"]), false),
                    (paths(&["a", "b"]), false),
                ],
            ),
            (
                paths(&["a"]),
                vec![
                    (ReadScope::Unknown, true),
                    (ReadScope::Unrestricted, true),
                    (paths(&[]), false),
                    (paths(&["a"]), false),
                    (paths(&["b"]), true),
                    (paths(&["a", "b"]), true),
                ],
            ),
            (
                paths(&[]),
                vec![
                    (ReadScope::Unknown, true),
                    (ReadScope::Unrestricted, true),
                    (paths(&[]), false),
                    (paths(&["a"]), true),
                ],
            ),
        ];
        for (sender, recipients) in rows {
            for (recipient, expected) in recipients {
                assert_eq!(
                    asymmetric(&sender, &recipient),
                    expected,
                    "sender {sender:?} recipient {recipient:?}"
                );
            }
        }
    }

    fn scope_strategy() -> impl Strategy<Value = ReadScope> {
        let alphabet = proptest::sample::select(vec!["a", "b", "c", "d"]);
        let to_scope = |items: Vec<&str>| paths(&items);
        let listed = proptest::collection::vec(alphabet, 0..4).prop_map(to_scope);
        prop_oneof![
            Just(ReadScope::Unknown),
            Just(ReadScope::Unrestricted),
            listed
        ]
    }

    fn authority_of(sender: &ReadScope) -> (bool, std::collections::HashSet<String>) {
        match sender {
            ReadScope::Unknown => (false, Default::default()),
            ReadScope::Unrestricted => (true, Default::default()),
            ReadScope::Paths(list) => (false, list.iter().cloned().collect()),
        }
    }

    fn reach_of(recipient: &ReadScope) -> (bool, std::collections::HashSet<String>) {
        match recipient {
            ReadScope::Paths(list) => (false, list.iter().cloned().collect()),
            _ => (true, Default::default()),
        }
    }

    proptest! {
        #[test]
        fn the_decision_equals_a_set_model_of_what_each_side_may_read(
            sender in scope_strategy(),
            recipient in scope_strategy(),
        ) {
            let (sender_all, sender_set) = authority_of(&sender);
            let (recipient_all, recipient_set) = reach_of(&recipient);
            let covered = sender_all || (!recipient_all && recipient_set.is_subset(&sender_set));
            prop_assert_eq!(asymmetric(&sender, &recipient), !covered);
        }

        #[test]
        fn a_read_scope_always_survives_a_json_round_trip(scope in scope_strategy()) {
            let json = serde_json::to_string(&scope).unwrap();
            prop_assert_eq!(serde_json::from_str::<ReadScope>(&json).unwrap(), scope);
        }
    }
}
