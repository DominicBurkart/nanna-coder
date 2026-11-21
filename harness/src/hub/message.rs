use super::lineage::{RootId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Globally unique id of one stored message.
///
/// ```
/// use harness::hub::message::MessageId;
///
/// assert_ne!(MessageId::new(), MessageId::new());
/// let id = MessageId::from_uuid(uuid::Uuid::nil());
/// assert_eq!(id.as_uuid(), uuid::Uuid::nil());
/// assert_eq!(MessageId::default().as_uuid().get_version_num(), 4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(Uuid);

impl MessageId {
    /// A fresh random id.
    ///
    /// ```
    /// assert_ne!(harness::hub::message::MessageId::new(), harness::hub::message::MessageId::new());
    /// ```
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing uuid.
    ///
    /// ```
    /// let id = harness::hub::message::MessageId::from_uuid(uuid::Uuid::nil());
    /// assert_eq!(id.as_uuid(), uuid::Uuid::nil());
    /// ```
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying uuid.
    ///
    /// ```
    /// let id = harness::hub::message::MessageId::from_uuid(uuid::Uuid::nil());
    /// assert!(id.as_uuid().is_nil());
    /// ```
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

/// Position of a message in the store's append order. Assigned by the
/// store, starting at 1, never reused.
///
/// ```
/// use harness::hub::message::Seq;
///
/// assert!(Seq::new(1) < Seq::new(2));
/// assert_eq!(Seq::new(7).get(), 7);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Seq(u64);

impl Seq {
    /// Wrap a raw sequence number.
    ///
    /// ```
    /// assert_eq!(harness::hub::message::Seq::new(3).get(), 3);
    /// ```
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    /// The raw sequence number.
    ///
    /// ```
    /// assert_eq!(harness::hub::message::Seq::new(0).get(), 0);
    /// ```
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Who a message is addressed to.
///
/// ```
/// use harness::hub::message::Recipient;
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let to = Recipient::Session { id: SessionId::from_task(TaskId("t".into())) };
/// assert_eq!(serde_json::to_string(&to).unwrap(), r#"{"to":"session","id":"t"}"#);
/// assert_eq!(serde_json::to_string(&Recipient::Parent).unwrap(), r#"{"to":"parent"}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Recipient {
    Session { id: SessionId },
    Parent,
    Children,
    Siblings,
    Root,
}

/// What a sender asks `msg_send` to deliver. Every field is agent-authored.
/// `Debug` prints lengths, never text.
///
/// ```
/// use harness::hub::message::{MessageKind, Recipient, SendRequest};
///
/// let req = SendRequest {
///     to: Recipient::Parent,
///     kind: MessageKind::Info,
///     subject: "status".into(),
///     body: "a secret plan".into(),
///     in_reply_to: None,
/// };
/// assert!(!format!("{req:?}").contains("secret"));
/// assert!(format!("{req:?}").contains("body_len: 13"));
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub to: Recipient,
    pub kind: MessageKind,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<MessageId>,
}

impl fmt::Debug for SendRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendRequest")
            .field("to", &self.to)
            .field("kind", &self.kind)
            .field("in_reply_to", &self.in_reply_to)
            .field("subject_len", &self.subject.len())
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// The ids of the stored messages, one per recipient.
///
/// ```
/// use harness::hub::message::SendReceipt;
///
/// let receipt = SendReceipt { ids: vec![], recipients: 0 };
/// assert_eq!(receipt.recipients, receipt.ids.len());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendReceipt {
    pub ids: Vec<MessageId>,
    pub recipients: usize,
}

/// How far a session's identity card lets it message: `None` forbids
/// `msg_send`, `Lineage` allows the parent, children, siblings and the root
/// mailbox, `Root` allows any session in the root. The order is the
/// permission order, so a narrowing card may only pick a value less than or
/// equal to its base, and a card that does not state a value gets `None`.
///
/// ```
/// use harness::hub::message::MessageReach;
///
/// assert!(MessageReach::None < MessageReach::Lineage && MessageReach::Lineage < MessageReach::Root);
/// assert_eq!(MessageReach::default(), MessageReach::None);
/// assert_eq!("lineage".parse::<MessageReach>(), Ok(MessageReach::Lineage));
/// assert_eq!(MessageReach::Root.as_str(), "root");
/// assert!("everyone".parse::<MessageReach>().is_err());
/// ```
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MessageReach {
    #[default]
    None,
    Lineage,
    Root,
}

impl MessageReach {
    /// Every value, in ascending permission order.
    ///
    /// ```
    /// use harness::hub::message::MessageReach;
    /// assert!(MessageReach::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    /// ```
    pub const ALL: [MessageReach; 3] = [
        MessageReach::None,
        MessageReach::Lineage,
        MessageReach::Root,
    ];

    /// Stable snake_case name, identical to the serde representation.
    ///
    /// ```
    /// use harness::hub::message::MessageReach;
    /// assert_eq!(MessageReach::None.as_str(), "none");
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
            MessageReach::None => "none",
            MessageReach::Lineage => "lineage",
            MessageReach::Root => "root",
        }
    }
}

impl fmt::Display for MessageReach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MessageReach {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MessageReach::ALL
            .into_iter()
            .find(|reach| reach.as_str() == s)
            .ok_or(())
    }
}

/// The purpose of a message.
///
/// ```
/// use harness::hub::message::MessageKind;
///
/// assert_eq!(serde_json::to_string(&MessageKind::StatusUpdate).unwrap(), "\"status_update\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Info,
    Question,
    Answer,
    Handoff,
    StatusUpdate,
}

/// Where a message is in its delivery life.
///
/// ```
/// use harness::hub::message::DeliveryState;
///
/// assert_eq!(serde_json::to_string(&DeliveryState::Quarantined).unwrap(), "\"quarantined\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Acked,
    Withheld,
    Quarantined,
    Expired,
}

/// Pointer to the outbound audit verdict that let the message through: the
/// audit log id and the content hash the verdict covered.
///
/// ```
/// use harness::hub::message::VerdictRef;
///
/// let verdict = VerdictRef { log_id: "a-1".into(), content_hash: 42 };
/// assert_eq!(serde_json::to_string(&verdict).unwrap(), r#"{"log_id":"a-1","content_hash":42}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRef {
    pub log_id: String,
    pub content_hash: u64,
}

/// One message addressed to one session. Fan-out is one record per
/// recipient. This is the one place message bodies are stored, so `Debug`
/// prints sizes and ids, never subject, body or sender-chosen text.
///
/// ```
/// use chrono::Utc;
/// use harness::hub::lineage::{RootId, SessionId};
/// use harness::hub::message::*;
/// use harness::task::TaskId;
///
/// let message = StoredMessage {
///     id: MessageId::new(),
///     seq: Seq::new(1),
///     from: SessionId::from_task(TaskId("a".into())),
///     from_identity: "coder".into(),
///     to: SessionId::from_task(TaskId("b".into())),
///     root: RootId::mcp(uuid::Uuid::nil()),
///     kind: MessageKind::Info,
///     subject: "hello".into(),
///     body: "a secret body".into(),
///     in_reply_to: None,
///     thread_depth: 0,
///     outbound_verdict: VerdictRef { log_id: "v".into(), content_hash: 1 },
///     state: DeliveryState::Pending,
///     sent_at: Utc::now(),
/// };
/// assert!(!format!("{message:?}").contains("secret"));
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: MessageId,
    pub seq: Seq,
    pub from: SessionId,
    pub from_identity: String,
    pub to: SessionId,
    pub root: RootId,
    pub kind: MessageKind,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<MessageId>,
    pub thread_depth: u8,
    pub outbound_verdict: VerdictRef,
    pub state: DeliveryState,
    pub sent_at: DateTime<Utc>,
}

impl fmt::Debug for StoredMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredMessage")
            .field("id", &self.id)
            .field("seq", &self.seq)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("kind", &self.kind)
            .field("in_reply_to", &self.in_reply_to)
            .field("thread_depth", &self.thread_depth)
            .field("state", &self.state)
            .field("sent_at", &self.sent_at)
            .field("subject_bytes", &self.subject.len())
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_reach_defaults_to_none_and_orders_by_permission() {
        assert_eq!(MessageReach::default(), MessageReach::None);
        assert!(MessageReach::None < MessageReach::Lineage);
        assert!(MessageReach::Lineage < MessageReach::Root);
        assert_eq!(MessageReach::Lineage.to_string(), "lineage");
        assert_eq!(
            serde_json::to_string(&MessageReach::Root).unwrap(),
            "\"root\""
        );
    }

    fn stored() -> StoredMessage {
        StoredMessage {
            id: MessageId::new(),
            seq: Seq::new(1),
            from: SessionId::from_task(crate::task::TaskId("a".into())),
            from_identity: "identity-SECRET".into(),
            to: SessionId::from_task(crate::task::TaskId("b".into())),
            root: RootId::mcp(Uuid::nil()),
            kind: MessageKind::Info,
            subject: "subject-SECRET".into(),
            body: "body-SECRET".into(),
            in_reply_to: None,
            thread_depth: 0,
            outbound_verdict: VerdictRef {
                log_id: "v".into(),
                content_hash: 1,
            },
            state: DeliveryState::Pending,
            sent_at: Utc::now(),
        }
    }

    #[test]
    fn ids_are_fresh_version_four_uuids_and_round_trip() {
        assert_ne!(MessageId::new(), MessageId::new());
        assert_eq!(MessageId::default().as_uuid().get_version_num(), 4);
        let id = MessageId::from_uuid(Uuid::from_u128(5));
        assert_eq!(id.as_uuid(), Uuid::from_u128(5));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<MessageId>(&json).unwrap(), id);
    }

    #[test]
    fn seq_orders_numerically() {
        assert!(Seq::new(2) < Seq::new(10));
        assert_eq!(Seq::new(7).get(), 7);
    }

    #[test]
    fn debug_of_a_stored_message_never_prints_text_fields() {
        let debug = format!("{:?}", stored());
        assert!(!debug.contains("SECRET"), "{debug}");
        assert!(debug.contains("body_bytes: 11"), "{debug}");
        assert!(debug.contains("subject_bytes: 14"), "{debug}");
    }

    #[test]
    fn a_stored_message_round_trips_through_json_with_every_field() {
        let message = stored();
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<StoredMessage>(&json).unwrap(),
            message
        );
    }

    #[test]
    fn recipients_use_the_documented_wire_shape() {
        let session = Recipient::Session {
            id: SessionId::from_task(crate::task::TaskId("t".into())),
        };
        assert_eq!(
            serde_json::to_string(&session).unwrap(),
            r#"{"to":"session","id":"t"}"#
        );
        for (recipient, wire) in [
            (Recipient::Parent, "parent"),
            (Recipient::Children, "children"),
            (Recipient::Siblings, "siblings"),
            (Recipient::Root, "root"),
        ] {
            assert_eq!(
                serde_json::to_string(&recipient).unwrap(),
                format!(r#"{{"to":"{wire}"}}"#)
            );
        }
    }
}
