use super::{
    fence_of, is_false, released_marker_valid, IDENTITY_RENDER_CAP, RECV_DEFAULT, RECV_MAX,
    RECV_PAGE_BYTES, SCAN_PAGE, SUBJECT_RENDER_CAP,
};
use crate::action_auditor::{MessageVerdict, NullOracle, RuleMessageAuditor};
use crate::auditor::ReasonCode;
use crate::hub::error::HubError;
use crate::hub::lineage::{RootId, SessionId};
use crate::hub::mailbox::MailboxId;
use crate::hub::message::{DeliveryState, MessageId, MessageKind, Seq, StoredMessage};
use crate::hub::send::{codes_of, hash_u64, MessageHub};
use crate::pr_tools::{untrusted_notice_as, FenceLabel};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use std::fmt;

/// What the operator asks the root mailbox for. No field carries text.
///
/// `after` is the last `Seq` the caller has seen: only messages with a higher `Seq` come back.
/// `ack_through` acknowledges the mailbox's `Delivered` messages up to that `Seq` first.
///
/// ```
/// use harness::hub::recv::RootRequest;
///
/// let req = RootRequest::default();
/// assert_eq!((req.max, req.after, req.ack_through), (10, None, None));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootRequest {
    pub max: u8,
    pub after: Option<Seq>,
    pub ack_through: Option<Seq>,
}

impl Default for RootRequest {
    fn default() -> Self {
        Self {
            max: RECV_DEFAULT,
            after: None,
            ack_through: None,
        }
    }
}

/// One message of the root mailbox as the operator's reader sees it. Every agent-authored string
/// is fenced as untrusted data and escaped; `Debug` prints sizes, never text. The operator has
/// no lineage, so there is no relation; `from_depth` is the sender's depth when the sender is
/// still known. `released_by_human` is serialised only when true.
#[derive(Clone, Serialize)]
pub struct RootMessage {
    pub id: MessageId,
    pub seq: Seq,
    pub from: SessionId,
    pub from_identity: String,
    pub from_depth: Option<u8>,
    pub kind: MessageKind,
    pub in_reply_to: Option<MessageId>,
    pub sent_at: DateTime<Utc>,
    pub subject: String,
    pub body: String,
    #[serde(skip_serializing_if = "is_false")]
    pub released_by_human: bool,
}

impl fmt::Debug for RootMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootMessage")
            .field("id", &self.id)
            .field("seq", &self.seq)
            .field("from_depth", &self.from_depth)
            .field("subject_bytes", &self.subject.len())
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// What `root_inbox` returns. `withheld` counts messages held back by the recipient-side review
/// (blocked or quarantined); it never says why or what they said.
#[derive(Clone, Serialize)]
pub struct RootPage {
    pub messages: Vec<RootMessage>,
    pub remaining: usize,
    pub withheld: usize,
    pub notice: String,
}

impl fmt::Debug for RootPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootPage")
            .field("messages", &self.messages)
            .field("remaining", &self.remaining)
            .field("withheld", &self.withheld)
            .finish_non_exhaustive()
    }
}

enum RootDecision {
    Deliver,
    Retire(DeliveryState, Vec<ReasonCode>),
}

struct RootJudged {
    message: StoredMessage,
    depth: Option<u8>,
    decision: RootDecision,
}

fn held_by_rules(message: &StoredMessage) -> RootDecision {
    let (subject, body) = (&message.subject, &message.body);
    let outcome = RuleMessageAuditor::new().review_with(subject, body, &[], &NullOracle);
    match outcome.verdict {
        MessageVerdict::Deliver => RootDecision::Deliver,
        held => {
            let quarantine = held.is_quarantine();
            let state = if quarantine {
                DeliveryState::Quarantined
            } else {
                DeliveryState::Withheld
            };
            RootDecision::Retire(state, codes_of(&held))
        }
    }
}

fn root_decision(message: &StoredMessage) -> RootDecision {
    let released = released_marker_valid(message);
    let covered =
        message.outbound_verdict.content_hash == hash_u64(&message.subject, &message.body);
    if released {
        RootDecision::Deliver
    } else if !covered {
        RootDecision::Retire(DeliveryState::Quarantined, vec![ReasonCode::Other])
    } else {
        held_by_rules(message)
    }
}

impl MessageHub {
    /// Read the root mailbox of `root`: the operator's side of `Recipient::Root`.
    ///
    /// Only messages with `to` equal to the mailbox of `root` AND `root` equal to `root` are
    /// considered, whatever the store hands back, and only `Pending` or `Delivered` ones with a
    /// `Seq` above `req.after`. Each is reviewed on the recipient side before it is shown: the
    /// rules layer is re-run over the stored (already redacted) subject and body, and the stored
    /// outbound verdict must still bind that text. A rules `Block` retires the message as
    /// `Withheld`, a rules `Quarantine` or a hash mismatch as `Quarantined` (the sink is told
    /// once); the caller gets a count, never the text. There is no model pass: the operator has
    /// no identity card, and the model reviews of record are the send-time outbound passes. A
    /// message carrying a valid release marker is shown as stored, with `released_by_human`, as
    /// `recv` does. The operator mark relaxes none of these checks.
    ///
    /// Reading is not destructive: a shown message becomes `Delivered` and is shown again by
    /// later calls until `after` moves past it or `ack_through` acknowledges it, so a client that
    /// crashed mid-read loses nothing. `ack_through` acknowledges this mailbox's messages only.
    /// Subject, body and sender identity are fenced and escaped exactly as in `recv`. A root that
    /// has no mailbox (a hand-built invalid one) is `NotVisible`; a root with no mail returns an
    /// empty page indistinguishable from any other empty root.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use harness::action_auditor::{MessageAuditor, RuleMessageAuditor};
    /// use harness::hub::limits::HubLimits;
    /// use harness::hub::lineage::RootId;
    /// use harness::hub::recv::RootRequest;
    /// use harness::hub::registry::SessionRegistry;
    /// use harness::hub::send::{MessageHub, QuarantineEvent, QuarantineSink};
    /// use harness::hub::store::{InMemoryMessageStore, InMemorySessionStore};
    /// use harness::leases::SimulatedClock;
    ///
    /// struct Quiet;
    /// impl QuarantineSink for Quiet {
    ///     fn quarantined(&self, _event: &QuarantineEvent) -> Result<(), String> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let sessions = Arc::new(InMemorySessionStore::new());
    /// let registry = Arc::new(SessionRegistry::new(sessions.clone()));
    /// let auditor: Arc<dyn MessageAuditor> = Arc::new(RuleMessageAuditor::new());
    /// let hub = MessageHub::with_sink(
    ///     sessions,
    ///     Arc::new(InMemoryMessageStore::new()),
    ///     Arc::new(auditor),
    ///     Arc::new(SimulatedClock::new(chrono::Utc::now())),
    ///     HubLimits::default(),
    ///     registry,
    ///     Arc::new(Quiet),
    /// );
    /// let root = RootId::plan("plan-0a1b").unwrap();
    /// let page = hub.root_inbox(&root, RootRequest::default()).unwrap();
    /// assert!(page.messages.is_empty());
    /// assert_eq!((page.remaining, page.withheld), (0, 0));
    /// ```
    pub fn root_inbox(&self, root: &RootId, req: RootRequest) -> Result<RootPage, HubError> {
        let mailbox = MailboxId::for_root(root).map_err(|_| HubError::NotVisible)?;
        if let Some(upto) = req.ack_through {
            self.messages.ack(mailbox.session().clone(), upto)?;
        }
        let max = usize::from(req.max.clamp(1, RECV_MAX));
        let queue = self.mailbox_queue(&mailbox, req.after)?;
        let total = queue.len();
        let in_root = self.sessions.in_root(root)?;
        let mut bytes = 0usize;
        let mut judged: Vec<RootJudged> = Vec::new();
        for message in queue.into_iter().take(max) {
            bytes = bytes.saturating_add(message.subject.len() + message.body.len());
            if !judged.is_empty() && bytes > RECV_PAGE_BYTES {
                break;
            }
            let sender = in_root.iter().find(|rec| rec.id == message.from);
            let depth = sender.map(|rec| rec.lineage.depth());
            let decision = root_decision(&message);
            judged.push(RootJudged {
                message,
                depth,
                decision,
            });
        }
        self.commit_root(judged, total)
    }

    fn mailbox_queue(
        &self,
        mailbox: &MailboxId,
        after: Option<Seq>,
    ) -> Result<Vec<StoredMessage>, HubError> {
        let me = mailbox.session();
        let mut found: Vec<StoredMessage> = Vec::new();
        let mut seen: HashSet<MessageId> = HashSet::new();
        let mut cursor = after;
        loop {
            let raw = self.messages.inbox(me.clone(), cursor, SCAN_PAGE)?;
            let last = raw.last().map(|message| message.seq);
            let full = raw.len() >= SCAN_PAGE;
            for message in raw {
                let mine = message.to == *me && message.root == *mailbox.root();
                let open = matches!(
                    message.state,
                    DeliveryState::Pending | DeliveryState::Delivered
                );
                let fresh = after.is_none_or(|floor| message.seq > floor);
                if mine && open && fresh && seen.insert(message.id) {
                    found.push(message);
                }
            }
            let advanced = last.is_some_and(|seq| cursor.is_none_or(|before| seq > before));
            if !(full && advanced) {
                break;
            }
            cursor = last;
        }
        found.sort_by_key(|message| message.seq);
        Ok(found)
    }

    fn commit_root(&self, judged: Vec<RootJudged>, total: usize) -> Result<RootPage, HubError> {
        let mut messages: Vec<RootMessage> = Vec::new();
        let mut withheld = 0usize;
        for item in judged {
            let step = match item.decision {
                RootDecision::Deliver => self
                    .show_root(&item.message, item.depth)
                    .map(|shown| messages.push(shown)),
                RootDecision::Retire(state, codes) => self
                    .retire(&item.message, state, &codes)
                    .map(|()| withheld += 1),
            };
            if let Err(error) = step {
                if messages.is_empty() && withheld == 0 {
                    return Err(error);
                }
                break;
            }
        }
        let remaining = total.saturating_sub(messages.len() + withheld);
        Ok(RootPage {
            messages,
            remaining,
            withheld,
            notice: format!(
                "{} {}",
                untrusted_notice_as(FenceLabel::AgentMessage),
                untrusted_notice_as(FenceLabel::AgentText)
            ),
        })
    }

    fn show_root(
        &self,
        message: &StoredMessage,
        depth: Option<u8>,
    ) -> Result<RootMessage, HubError> {
        self.messages
            .set_state(message.id, DeliveryState::Delivered)?;
        let body_cap = self.limits.body_max;
        Ok(RootMessage {
            id: message.id,
            seq: message.seq,
            from: message.from.clone(),
            from_identity: fence_of(
                FenceLabel::AgentText,
                &message.from_identity,
                IDENTITY_RENDER_CAP,
            ),
            from_depth: depth,
            kind: message.kind,
            in_reply_to: message.in_reply_to,
            sent_at: message.sent_at,
            subject: fence_of(
                FenceLabel::AgentMessage,
                &message.subject,
                SUBJECT_RENDER_CAP,
            ),
            body: fence_of(FenceLabel::AgentMessage, &message.body, body_cap),
            released_by_human: released_marker_valid(message),
        })
    }
}
