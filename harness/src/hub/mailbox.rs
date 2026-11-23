use std::fmt;

use uuid::Uuid;

use super::lineage::{RootId, SessionId, SessionIdError};
use crate::task::TaskId;

const SIGIL: char = '@';
const MAILBOX_PREFIX: &str = "@mailbox/";
const TASK_ESCAPE: &str = "@task/";

pub(super) fn escape_task(raw: String) -> String {
    match raw.starts_with(SIGIL) {
        true => format!("{TASK_ESCAPE}{raw}"),
        false => raw,
    }
}

pub(super) fn wire_is_valid(raw: &str) -> bool {
    let mailbox = raw.strip_prefix(MAILBOX_PREFIX);
    let escaped = raw.strip_prefix(TASK_ESCAPE);
    match (mailbox, escaped) {
        (Some(rest), _) => decode(rest).is_some(),
        (None, Some(rest)) => rest.starts_with(SIGIL),
        (None, None) => !raw.starts_with(SIGIL),
    }
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn nibble(byte: u8) -> Option<u8> {
    let digit = char::from(byte).to_digit(16)?;
    u8::try_from(digit).ok()
}

fn byte_of(pair: &[u8]) -> Option<u8> {
    match pair {
        [high, low] => Some(nibble(*high)? * 16 + nibble(*low)?),
        _ => None,
    }
}

fn unhex(text: &str) -> Option<String> {
    let pairs = text.as_bytes().chunks(2);
    let bytes: Option<Vec<u8>> = pairs.map(byte_of).collect();
    let decoded = String::from_utf8(bytes?).ok()?;
    (hex(&decoded) == text).then_some(decoded)
}

fn encode(root: &RootId) -> String {
    let tail = match root {
        RootId::Mcp { id } => format!("mcp/{}", id.hyphenated()),
        RootId::Plan { id } => format!("plan/{id}"),
        RootId::Backlog { repo, issue } => format!("backlog/{repo}#{issue}"),
        RootId::Legacy { task } => format!("legacy/{}", hex(&task.0)),
    };
    format!("{MAILBOX_PREFIX}{tail}")
}

fn backlog(tail: &str) -> Option<RootId> {
    let (repo, issue) = tail.rsplit_once('#')?;
    let issue = issue.parse::<u64>().ok()?;
    RootId::backlog(repo, issue).ok()
}

fn decode(rest: &str) -> Option<RootId> {
    let (kind, tail) = rest.split_once('/')?;
    let root = match kind {
        "mcp" => Uuid::parse_str(tail).ok().map(RootId::mcp),
        "plan" => RootId::plan(tail).ok(),
        "backlog" => backlog(tail),
        "legacy" => unhex(tail).map(|task| RootId::legacy(TaskId(task))),
        _ => None,
    }?;
    (encode(&root) == format!("{MAILBOX_PREFIX}{rest}")).then_some(root)
}

/// The address of a root's mailbox: a synthetic [`SessionId`] derived from the [`RootId`].
///
/// It is never registered in the session store and never enters the binding table.
/// [`SessionId::from_task`] escapes every task id that starts with `@`, so no task id can equal
/// a mailbox id, and a mailbox id is built only here. The id is a shape, not an authority:
/// whoever can name text that parses as a mailbox id gets nothing from the shape alone, and the
/// operator flag is computed from [`is_mailbox`] on the stored sender, never from a name.
///
/// ```
/// use harness::hub::lineage::{RootId, SessionId};
/// use harness::hub::mailbox::{is_mailbox, MailboxId};
/// use harness::task::TaskId;
///
/// let root = RootId::plan("plan-0a1b").unwrap();
/// let mailbox = MailboxId::for_root(&root).unwrap();
/// assert_eq!(mailbox.session().to_string(), "@mailbox/plan/plan-0a1b");
/// assert_eq!(mailbox.root(), &root);
/// assert!(is_mailbox(mailbox.session()));
/// let lookalike = SessionId::from_task(TaskId("@mailbox/plan/plan-0a1b".into()));
/// assert!(!is_mailbox(&lookalike));
/// assert_ne!(&lookalike, mailbox.session());
/// ```
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MailboxId {
    session: SessionId,
    root: RootId,
}

impl MailboxId {
    /// The mailbox of `root`. A root whose public fields were filled by hand and that the
    /// validating constructors would refuse has no mailbox.
    ///
    /// ```
    /// use harness::hub::lineage::RootId;
    /// use harness::hub::mailbox::MailboxId;
    ///
    /// let forged = RootId::Plan { id: "NOT VALID".into() };
    /// assert!(MailboxId::for_root(&forged).is_err());
    /// ```
    pub fn for_root(root: &RootId) -> Result<Self, SessionIdError> {
        let session = SessionId::from_mailbox(encode(root));
        Self::of(&session).ok_or(SessionIdError)
    }

    /// The mailbox `session` names, or `None` for every task-derived id.
    ///
    /// ```
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::mailbox::MailboxId;
    /// use harness::task::TaskId;
    ///
    /// assert!(MailboxId::of(&SessionId::from_task(TaskId::new())).is_none());
    /// ```
    pub fn of(session: &SessionId) -> Option<Self> {
        let rest = session.text().strip_prefix(MAILBOX_PREFIX)?;
        let root = decode(rest)?;
        Some(Self {
            session: session.clone(),
            root,
        })
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn into_session(self) -> SessionId {
        self.session
    }

    pub fn root(&self) -> &RootId {
        &self.root
    }
}

impl fmt::Debug for MailboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MailboxId")
    }
}

/// Whether `session` is a root mailbox address (canonical form, not just a prefix).
///
/// ```
/// use harness::hub::lineage::{RootId, SessionId};
/// use harness::hub::mailbox::{is_mailbox, MailboxId};
/// use harness::task::TaskId;
///
/// let mailbox = MailboxId::for_root(&RootId::mcp(uuid::Uuid::nil())).unwrap();
/// assert!(is_mailbox(mailbox.session()));
/// assert!(!is_mailbox(&SessionId::from_task(TaskId::new())));
/// ```
pub fn is_mailbox(session: &SessionId) -> bool {
    MailboxId::of(session).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::lineage::{Lineage, SessionBinding};
    use crate::hub::router::BindingTable;
    use proptest::prelude::*;

    const TOKEN: &str = "PLANTED-TOKEN-7c1f";

    fn mcp(n: u128) -> RootId {
        RootId::mcp(Uuid::from_u128(n))
    }

    fn plan(id: &str) -> RootId {
        RootId::plan(id).unwrap()
    }

    fn backlog_root(repo: &str, issue: u64) -> RootId {
        RootId::backlog(repo, issue).unwrap()
    }

    fn legacy(task: &str) -> RootId {
        RootId::legacy(TaskId(task.to_string()))
    }

    fn mailbox_of(root: &RootId) -> MailboxId {
        MailboxId::for_root(root).unwrap()
    }

    fn text_of(root: &RootId) -> String {
        mailbox_of(root).session().to_string()
    }

    fn parse(text: &str) -> Result<SessionId, SessionIdError> {
        text.parse()
    }

    fn json_of(text: &str) -> String {
        serde_json::to_string(text).unwrap()
    }

    fn from_json(text: &str) -> Result<SessionId, serde_json::Error> {
        serde_json::from_str(&json_of(text))
    }

    fn all_four() -> Vec<RootId> {
        vec![
            mcp(1),
            plan("plan-0a1b"),
            backlog_root("example/repo", 7),
            legacy("t-1"),
        ]
    }

    fn backlog_of((repo, issue): (String, u64)) -> Option<RootId> {
        RootId::backlog(repo, issue).ok()
    }

    fn root_strategy() -> impl Strategy<Value = RootId> {
        let mcps = any::<u8>().prop_map(|n| mcp(u128::from(n % 3)));
        let plans = "[ab-]{1,3}".prop_map(|id| plan(&id));
        let repos = "[ab._-]{1,2}/[ab]{1,2}";
        let backlogs = (repos, 0u64..3).prop_filter_map("invalid repo", backlog_of);
        let legacies = "[a@/#-]{0,3}".prop_map(|task| legacy(&task));
        prop_oneof![mcps, plans, backlogs, legacies]
    }

    #[test]
    fn each_root_kind_has_its_documented_address() {
        let nil = "@mailbox/mcp/00000000-0000-0000-0000-000000000000";
        assert_eq!(text_of(&mcp(0)), nil);
        assert_eq!(text_of(&plan("plan-ab")), "@mailbox/plan/plan-ab");
        let seven = text_of(&backlog_root("example/repo", 7));
        assert_eq!(seven, "@mailbox/backlog/example/repo#7");
        assert_eq!(text_of(&legacy("t-1")), "@mailbox/legacy/742d31");
        assert_eq!(text_of(&legacy("")), "@mailbox/legacy/");
    }

    #[test]
    fn the_address_is_deterministic_and_names_its_root() {
        for root in all_four() {
            let first = mailbox_of(&root);
            let second = mailbox_of(&root);
            assert_eq!(first, second);
            assert_eq!(first.root(), &root);
            assert!(is_mailbox(first.session()));
            assert_eq!(first.clone().into_session(), first.session().clone());
        }
    }

    #[test]
    fn different_roots_have_different_addresses() {
        let all = all_four();
        let texts: std::collections::HashSet<String> = all.iter().map(text_of).collect();
        assert_eq!(texts.len(), all.len());
        assert_ne!(text_of(&plan("a")), text_of(&legacy("a")));
        assert_ne!(text_of(&mcp(1)), text_of(&mcp(2)));
        let seven = text_of(&backlog_root("example/repo", 7));
        let eight = text_of(&backlog_root("example/repo", 8));
        assert_ne!(seven, eight);
    }

    #[test]
    fn a_legacy_task_that_contains_the_separators_stays_distinct() {
        let tricky = legacy("plan/x#1/@mailbox/");
        let other = plan("x");
        assert_ne!(text_of(&tricky), text_of(&other));
        assert_eq!(mailbox_of(&tricky).root(), &tricky);
    }

    #[test]
    fn a_hand_filled_invalid_root_has_no_mailbox() {
        let bad_plan = RootId::Plan {
            id: "NOT VALID".into(),
        };
        let bad_repo = RootId::Backlog {
            repo: "no-slash".into(),
            issue: 1,
        };
        assert!(MailboxId::for_root(&bad_plan).is_err());
        assert!(MailboxId::for_root(&bad_repo).is_err());
    }

    #[test]
    fn a_task_id_that_looks_like_a_mailbox_is_escaped_not_accepted() {
        for root in all_four() {
            let mailbox = mailbox_of(&root);
            let lookalike = SessionId::from_task(TaskId(mailbox.session().to_string()));
            assert!(!is_mailbox(&lookalike));
            assert!(MailboxId::of(&lookalike).is_none());
            assert_ne!(&lookalike, mailbox.session());
            assert!(lookalike.to_string().starts_with("@task/@mailbox/"));
        }
    }

    #[test]
    fn escaped_task_ids_keep_their_text_and_survive_every_form() {
        let sigil = SessionId::from_task(TaskId("@x".into()));
        let twice = SessionId::from_task(TaskId("@task/@x".into()));
        assert_eq!(sigil.to_string(), "@task/@x");
        assert_eq!(twice.to_string(), "@task/@task/@x");
        assert_ne!(sigil, twice);
        assert_eq!(parse(&sigil.to_string()), Ok(sigil.clone()));
        assert_eq!(from_json(&sigil.to_string()).unwrap(), sigil);
        let plain = SessionId::from_task(TaskId("a@b".into()));
        assert_eq!(plain.to_string(), "a@b");
    }

    #[test]
    fn plain_task_ids_are_unchanged() {
        for plain in ["t-1", "", "x@y", "mailbox/plan/a", "task/@x"] {
            let id = SessionId::from_task(TaskId(plain.to_string()));
            assert_eq!(id.to_string(), plain);
            assert_eq!(parse(plain), Ok(id.clone()));
            assert_eq!(from_json(plain).unwrap(), id);
        }
    }

    #[test]
    fn uuid_task_ids_are_never_mailboxes() {
        for _ in 0..16 {
            let id = SessionId::from_task(TaskId::new());
            assert!(!is_mailbox(&id));
        }
        let nil = SessionId::from_task(TaskId(Uuid::nil().to_string()));
        assert!(!is_mailbox(&nil));
    }

    #[test]
    fn serde_display_and_fromstr_agree_for_every_mailbox() {
        for root in all_four() {
            let mailbox = mailbox_of(&root);
            let session = mailbox.session();
            let json = serde_json::to_string(session).unwrap();
            assert_eq!(json, json_of(&session.to_string()));
            let back: SessionId = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, session);
            assert_eq!(parse(&session.to_string()).as_ref(), Ok(session));
            let again = MailboxId::of(&back).unwrap();
            assert_eq!(again, mailbox);
        }
    }

    #[test]
    fn non_canonical_or_forged_at_forms_are_refused_by_every_parser() {
        let upper = "@mailbox/mcp/00000000-0000-0000-0000-00000000000A";
        let simple = "@mailbox/mcp/00000000000000000000000000000000";
        let refused = [
            "@mailbox/",
            "@mailbox/garbage",
            "@mailbox/mcp",
            "@mailbox/mcp/",
            "@mailbox/mcp/not-a-uuid",
            upper,
            simple,
            "@mailbox/plan/",
            "@mailbox/plan/Plan-A",
            "@mailbox/backlog/example/repo",
            "@mailbox/backlog/example/repo#",
            "@mailbox/backlog/example/repo#07",
            "@mailbox/backlog/example/repo#+7",
            "@mailbox/backlog/norepo#7",
            "@mailbox/legacy/7",
            "@mailbox/legacy/ABCD",
            "@mailbox/legacy/ff",
            "@mailbox/legacy/7g",
            "@mailbox/unknown/x",
            "@mailbox/plan/a/b",
            "@x",
            "@",
            "@task/",
            "@task/plain",
            "@Mailbox/plan/a",
            "@task",
        ];
        for text in refused {
            assert_eq!(parse(text), Err(SessionIdError), "{text}");
            assert!(from_json(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_refusal_never_echoes_the_text() {
        let text = format!("@{TOKEN}");
        let parsed = parse(&text).unwrap_err();
        let shown = format!("{parsed} {parsed:?}");
        let from_serde = from_json(&text).unwrap_err().to_string();
        assert!(!shown.contains(TOKEN));
        assert!(!from_serde.contains(TOKEN));
    }

    #[test]
    fn debug_prints_the_kind_and_no_text() {
        let roots = [
            plan(&format!("plan-{}", TOKEN.to_lowercase())),
            backlog_root(&format!("{TOKEN}/{TOKEN}"), 4),
            legacy(TOKEN),
            mcp(0x7c1f),
        ];
        for root in roots {
            let mailbox = mailbox_of(&root);
            assert_eq!(format!("{mailbox:?}"), "MailboxId");
            assert_eq!(format!("{:?}", mailbox.session()), "SessionId(Mailbox)");
            assert_eq!(format!("{:#?}", mailbox.session()), "SessionId(Mailbox)");
        }
    }

    #[test]
    fn debug_of_a_task_session_is_unchanged() {
        let id = SessionId::from_task(TaskId("t-1".into()));
        assert_eq!(format!("{id:?}"), "SessionId(TaskId(\"t-1\"))");
    }

    #[test]
    fn the_binding_table_never_holds_a_mailbox() {
        let table = BindingTable::new();
        let mailbox = mailbox_of(&mcp(9));
        let card = crate::identity::example();
        let lineage = Lineage::root_child(mcp(9));
        let task = TaskId(mailbox.session().to_string());
        let binding = SessionBinding::from_audited(&card, task, lineage);
        assert!(!is_mailbox(binding.session()));
        let _guard = table.enter(binding);
        assert_eq!(table.len(), 1);
        assert!(table.binding(mailbox.session()).is_none());
    }

    proptest! {
        #[test]
        fn addresses_are_equal_exactly_when_the_roots_are(a in root_strategy(), b in root_strategy()) {
            let same_address = text_of(&a) == text_of(&b);
            prop_assert_eq!(same_address, a == b);
        }

        #[test]
        fn the_address_names_back_its_root(root in root_strategy()) {
            let mailbox = mailbox_of(&root);
            let back = MailboxId::of(mailbox.session()).unwrap();
            prop_assert_eq!(back.root(), &root);
            let text = mailbox.session().to_string();
            prop_assert_eq!(parse(&text).unwrap(), mailbox.session().clone());
        }

        #[test]
        fn no_task_id_ever_makes_a_mailbox(raw in "(@mailbox/|@task/|@)?[ -~]{0,30}") {
            let id = SessionId::from_task(TaskId(raw));
            prop_assert!(!is_mailbox(&id));
            let text = id.to_string();
            prop_assert_eq!(parse(&text).unwrap(), id.clone());
            prop_assert_eq!(from_json(&text).unwrap(), id);
        }

        #[test]
        fn a_task_id_spelled_like_any_address_differs_from_it(root in root_strategy()) {
            let mailbox = mailbox_of(&root);
            let spelled = SessionId::from_task(TaskId(mailbox.session().to_string()));
            prop_assert_ne!(&spelled, mailbox.session());
            prop_assert!(!is_mailbox(&spelled));
        }

        #[test]
        fn only_canonical_at_text_parses(raw in "@(mailbox/|task/)?[ -~]{0,30}") {
            if let Ok(id) = parse(&raw) {
                let canonical_mailbox = is_mailbox(&id);
                let escaped_task = raw.starts_with("@task/@");
                prop_assert!(canonical_mailbox || escaped_task);
                prop_assert_eq!(id.to_string(), raw);
            }
        }
    }
}
