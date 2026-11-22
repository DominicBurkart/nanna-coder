use super::error::HubError;
use super::lineage::SessionBinding;
use super::message::{MessageReach, Recipient};
use super::session::SessionRecord;

/// How a session stands to the sender, by lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Parent,
    Child,
    Sibling,
    Other,
}

/// The relation of `other` to `me`.
///
/// ```
/// use harness::hub::reach::Relation;
/// assert_ne!(Relation::Parent, Relation::Child);
/// ```
pub fn relation_of(me: &SessionBinding, other: &SessionRecord) -> Relation {
    if me.lineage().parent() == Some(&other.id) {
        Relation::Parent
    } else if other.lineage.parent() == Some(me.session()) {
        Relation::Child
    } else if other.lineage.parent() == me.lineage().parent() {
        Relation::Sibling
    } else {
        Relation::Other
    }
}

fn allowed(reach: MessageReach, relation: Relation) -> bool {
    reach == MessageReach::Root || relation != Relation::Other
}

/// The active recipients `to` names, from `in_root`, the sessions of the sender's own root.
/// An unknown id is [`HubError::NotVisible`], exactly as a session of another root would be,
/// because `in_root` never holds one.
pub(crate) fn resolve(
    sender: &SessionBinding,
    to: &Recipient,
    in_root: &[SessionRecord],
    fanout_max: usize,
) -> Result<Vec<SessionRecord>, HubError> {
    let reach = sender.identity().scope.message_reach;
    if reach == MessageReach::None {
        return Err(HubError::ReachDenied(reach));
    }
    let others = in_root.iter().filter(|rec| rec.id != *sender.session());
    let picked: Vec<&SessionRecord> = match to {
        Recipient::Session { id } => {
            if id == sender.session() {
                return Err(HubError::ReachDenied(reach));
            }
            let found = others.into_iter().find(|rec| rec.id == *id);
            let rec = found.ok_or(HubError::NotVisible)?;
            if !allowed(reach, relation_of(sender, rec)) {
                return Err(HubError::ReachDenied(reach));
            }
            if !rec.status.is_active() {
                return Err(HubError::RecipientInactive);
            }
            vec![rec]
        }
        Recipient::Parent => others
            .filter(|rec| relation_of(sender, rec) == Relation::Parent)
            .collect(),
        Recipient::Children => others
            .filter(|rec| relation_of(sender, rec) == Relation::Child)
            .collect(),
        Recipient::Siblings => others
            .filter(|rec| relation_of(sender, rec) == Relation::Sibling)
            .collect(),
        Recipient::Root => Vec::new(),
    };
    let active: Vec<SessionRecord> = picked
        .into_iter()
        .filter(|rec| rec.status.is_active())
        .cloned()
        .collect();
    if active.is_empty() {
        return Err(HubError::NoRecipient);
    }
    if active.len() > fanout_max {
        return Err(HubError::TooLarge {
            field: "recipients",
            max: fanout_max,
        });
    }
    Ok(active)
}
