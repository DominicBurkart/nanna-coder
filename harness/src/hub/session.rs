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
/// use harness::hub::session::{SessionKind, SessionRecord, SessionStatus};
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
}
