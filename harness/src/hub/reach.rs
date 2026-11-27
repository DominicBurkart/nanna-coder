use super::error::HubError;
use super::lineage::{SessionBinding, SessionId};
use super::mailbox::{operator_scope, MailboxId};
use super::message::{MessageReach, Recipient};
use super::send::scope_of;
use super::session::SessionRecord;
use crate::action_auditor::RecipientScope;

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

/// Where a send goes: active sessions of the sender's root, or the root mailbox.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    Sessions(Vec<SessionRecord>),
    Mailbox(MailboxId),
}

impl Target {
    pub(crate) fn ids(&self) -> Vec<SessionId> {
        match self {
            Target::Sessions(to) => to.iter().map(|rec| rec.id.clone()).collect(),
            Target::Mailbox(mailbox) => vec![mailbox.session().clone()],
        }
    }

    pub(crate) fn scopes(&self) -> Vec<RecipientScope> {
        match self {
            Target::Sessions(to) => to.iter().map(scope_of).collect(),
            Target::Mailbox(mailbox) => vec![operator_scope(mailbox)],
        }
    }

    pub(crate) fn sessions(&self) -> &[SessionRecord] {
        match self {
            Target::Sessions(to) => to,
            Target::Mailbox(_) => &[],
        }
    }
}

fn names_mailbox(sender: &SessionBinding, to: &Recipient) -> bool {
    let top = sender.lineage().parent().is_none();
    matches!(to, Recipient::Root) || (matches!(to, Recipient::Parent) && top)
}

/// The target `to` names: the root mailbox for [`Recipient::Root`] and for the parent of a
/// top-level session (when the card's reach is not `none`), otherwise exactly what [`resolve`]
/// returns. The mailbox is always that of the sender's own root, taken from its lineage.
pub(crate) fn resolve_target(
    sender: &SessionBinding,
    to: &Recipient,
    in_root: &[SessionRecord],
    fanout_max: usize,
) -> Result<Target, HubError> {
    let reach = sender.identity().scope.message_reach;
    if reach != MessageReach::None && names_mailbox(sender, to) {
        let root = sender.lineage().root();
        let mailbox = MailboxId::for_root(root).map_err(|_| HubError::NoRecipient)?;
        return Ok(Target::Mailbox(mailbox));
    }
    let sessions = resolve(sender, to, in_root, fanout_max)?;
    Ok(Target::Sessions(sessions))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_mailbox_target_is_built_only_in_the_target_resolver_from_the_senders_own_root() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hub");
        let mut scanned = 0;
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with(".rs") {
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("\r\n", "\n");
                let production = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
                for line in production.lines() {
                    let code = !line.trim_start().starts_with("///");
                    if code && line.contains("MailboxId::for_root(") {
                        found.push(format!("{name}: {}", line.trim()));
                    }
                }
                scanned += 1;
            }
        }
        assert!(scanned >= 10);
        let own = "let mailbox = MailboxId::for_root(root).map_err(|_| HubError::NoRecipient)?;";
        found.sort();
        let release = "operator.rs: let mailbox = MailboxId::for_root(&message.root);";
        let operator =
            "let mailbox = MailboxId::for_root(root).map_err(|_| HubError::NotVisible)?;";
        let expected = vec![
            release.to_string(),
            format!("reach.rs: {own}"),
            format!("send.rs: {operator}"),
        ];
        assert_eq!(found, expected);
        let source = std::fs::read_to_string(dir.join("reach.rs")).unwrap();
        let source = source.replace("\r\n", "\n");
        let production = source.split("\n#[cfg(test)]\nmod ").next().unwrap();
        assert!(production.contains("let root = sender.lineage().root();"));
    }
}
