use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::mailbox;
use crate::auditor::Allowed;
use crate::identity::AgentIdentity;
use crate::task::TaskId;

pub const MAX_DEPTH: u8 = 8;

pub const MAX_PLAN_ID_LEN: usize = 80;

const MAX_REPO_LEN: usize = 140;

const MCP_PREFIX: &str = "mcp-";
const PLAN_PREFIX: &str = "plan-";
const BACKLOG_PREFIX: &str = "backlog-";

/// A spawned subagent's id. Same value as its [`TaskId`] for every task id that does not start
/// with `@`.
///
/// `@` is reserved: [`SessionId::from_task`] prefixes a task id that starts with it, so no task
/// id can equal the id of a root mailbox (see [`crate::hub::mailbox`]). Serde, `Display` and
/// `FromStr` use one form and refuse any `@` text that is neither a canonical mailbox id nor an
/// escaped task id.
///
/// ```
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let task = TaskId("t-1".into());
/// let session = SessionId::from_task(task.clone());
/// assert_eq!(session.task_id(), &task);
/// assert_eq!(session.to_string(), "t-1");
/// assert_eq!("t-1".parse::<SessionId>(), Ok(session));
/// ```
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct SessionId(TaskId);

/// Text that is not the form of a task id or of a root mailbox id.
///
/// ```
/// use harness::hub::lineage::SessionId;
///
/// let refused = "@forged".parse::<SessionId>();
/// assert!(refused.is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a session id")]
pub struct SessionIdError;

impl SessionId {
    pub fn from_task(task: TaskId) -> Self {
        Self(TaskId(mailbox::escape_task(task.0)))
    }

    pub(super) fn from_mailbox(text: String) -> Self {
        Self(TaskId(text))
    }

    pub(super) fn text(&self) -> &str {
        &(self.0).0
    }

    /// The task id this session was built from. For an id that starts with `@` this is the
    /// escaped form, not the original.
    pub fn task_id(&self) -> &TaskId {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::fmt::Debug for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match mailbox::is_mailbox(self) {
            true => f.write_str("SessionId(Mailbox)"),
            false => f.debug_tuple("SessionId").field(&self.0).finish(),
        }
    }
}

impl std::str::FromStr for SessionId {
    type Err = SessionIdError;

    fn from_str(raw: &str) -> Result<Self, SessionIdError> {
        match mailbox::wire_is_valid(raw) {
            true => Ok(Self(TaskId(raw.to_string()))),
            false => Err(SessionIdError),
        }
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Why a lineage could not be built or accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LineageError {
    #[error("lineage deeper than {0}")]
    TooDeep(u8),
    #[error("plan root id is not [a-z0-9-]{{1,80}}")]
    InvalidPlanId,
    #[error("backlog root names an invalid repository or issue")]
    InvalidBacklog,
    #[error("parent id names a malformed root")]
    InvalidRoot,
    #[error("lineage depth and parent disagree")]
    Inconsistent,
}

/// The external entry point a session tree hangs from.
///
/// Values are built only through the validating constructors or by
/// deserializing, which applies the same validation.
///
/// ```
/// use harness::hub::lineage::{LineageError, RootId};
///
/// assert!(RootId::plan("plan-0a1b").is_ok());
/// assert_eq!(RootId::plan("Plan 1"), Err(LineageError::InvalidPlanId));
/// let json = serde_json::to_string(&RootId::backlog("example/repo", 7).unwrap()).unwrap();
/// assert_eq!(json, r#"{"kind":"backlog","repo":"example/repo","issue":7}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RootId {
    Mcp { id: Uuid },
    Plan { id: String },
    Backlog { repo: String, issue: u64 },
    Legacy { task: TaskId },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RootWire {
    Mcp { id: Uuid },
    Plan { id: String },
    Backlog { repo: String, issue: u64 },
    Legacy { task: TaskId },
}

fn valid_plan_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PLAN_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_repo(repo: &str) -> bool {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    if repo.len() > MAX_REPO_LEN {
        return false;
    }
    let mut parts = repo.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if part_ok(a) && part_ok(b))
}

impl RootId {
    pub fn mcp(id: Uuid) -> Self {
        Self::Mcp { id }
    }

    pub fn plan(id: impl Into<String>) -> Result<Self, LineageError> {
        let id = id.into();
        if !valid_plan_id(&id) {
            return Err(LineageError::InvalidPlanId);
        }
        Ok(Self::Plan { id })
    }

    pub fn backlog(repo: impl Into<String>, issue: u64) -> Result<Self, LineageError> {
        let repo = repo.into();
        if !valid_repo(&repo) {
            return Err(LineageError::InvalidBacklog);
        }
        Ok(Self::Backlog { repo, issue })
    }

    pub fn legacy(task: TaskId) -> Self {
        Self::Legacy { task }
    }
}

impl TryFrom<RootWire> for RootId {
    type Error = LineageError;

    fn try_from(wire: RootWire) -> Result<Self, LineageError> {
        match wire {
            RootWire::Mcp { id } => Ok(Self::mcp(id)),
            RootWire::Plan { id } => Self::plan(id),
            RootWire::Backlog { repo, issue } => Self::backlog(repo, issue),
            RootWire::Legacy { task } => Ok(Self::legacy(task)),
        }
    }
}

impl<'de> Deserialize<'de> for RootId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RootId::try_from(RootWire::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Where a session sits: its root, its parent session and its depth.
///
/// Fields are private. Only [`Lineage::root_child`] and [`Lineage::child_of`]
/// construct a value, and deserialization re-checks that depth and parent
/// agree, so a stored lineage cannot claim a depth it does not have.
///
/// ```
/// use harness::hub::lineage::{Lineage, RootId, SessionId};
/// use harness::task::TaskId;
/// use uuid::Uuid;
///
/// let root = Lineage::root_child(RootId::mcp(Uuid::nil()));
/// assert_eq!(root.depth(), 1);
/// assert!(root.parent().is_none());
/// let child = Lineage::child_of(SessionId::from_task(TaskId("a".into())), &root).unwrap();
/// assert_eq!(child.depth(), 2);
/// assert_eq!(child.root(), root.root());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lineage {
    root: RootId,
    parent: Option<SessionId>,
    depth: u8,
}

#[derive(Deserialize)]
struct LineageWire {
    root: RootId,
    parent: Option<SessionId>,
    depth: u8,
}

impl<'de> Deserialize<'de> for Lineage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = LineageWire::deserialize(deserializer)?;
        let consistent = matches!(
            (wire.depth, &wire.parent),
            (1, None) | (2..=MAX_DEPTH, Some(_))
        );
        if !consistent {
            return Err(serde::de::Error::custom(LineageError::Inconsistent));
        }
        Ok(Self {
            root: wire.root,
            parent: wire.parent,
            depth: wire.depth,
        })
    }
}

impl Lineage {
    pub fn root_child(root: RootId) -> Self {
        Self {
            root,
            parent: None,
            depth: 1,
        }
    }

    pub fn child_of(parent: SessionId, parent_lineage: &Lineage) -> Result<Self, LineageError> {
        let depth = parent_lineage.depth.saturating_add(1);
        if depth > MAX_DEPTH {
            return Err(LineageError::TooDeep(MAX_DEPTH));
        }
        Ok(Self {
            root: parent_lineage.root.clone(),
            parent: Some(parent),
            depth,
        })
    }

    pub fn root(&self) -> &RootId {
        &self.root
    }

    pub fn parent(&self) -> Option<&SessionId> {
        self.parent.as_ref()
    }

    pub fn depth(&self) -> u8 {
        self.depth
    }
}

/// The only authority the hub accepts for "who is calling".
pub struct SessionBinding {
    session: SessionId,
    lineage: Lineage,
    identity: Arc<AgentIdentity>,
}

impl SessionBinding {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_proof(allowed: &Allowed, task: TaskId, lineage: Lineage) -> Self {
        Self::from_audited(allowed.identity(), task, lineage)
    }

    pub(crate) fn from_audited(identity: &AgentIdentity, task: TaskId, lineage: Lineage) -> Self {
        Self {
            session: SessionId::from_task(task),
            lineage,
            identity: Arc::new(identity.clone()),
        }
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn lineage(&self) -> &Lineage {
        &self.lineage
    }

    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }
}

/// Map the id of the task that asked for a spawn to a lineage.
///
/// `Ok(None)` means the parent is not a recognised entry point or known
/// session; the caller gives the new task its own `Legacy` root.
pub(crate) fn derive_lineage(
    parent_id: &str,
    lookup: &dyn Fn(&TaskId) -> Option<Lineage>,
) -> Result<Option<Lineage>, LineageError> {
    if let Some(rest) = parent_id.strip_prefix(MCP_PREFIX) {
        if rest.len() != 36 {
            return Err(LineageError::InvalidRoot);
        }
        let id = Uuid::parse_str(rest).map_err(|_| LineageError::InvalidRoot)?;
        return Ok(Some(Lineage::root_child(RootId::mcp(id))));
    }
    if parent_id.starts_with(PLAN_PREFIX) {
        return Ok(Some(Lineage::root_child(RootId::plan(parent_id)?)));
    }
    if let Some(rest) = parent_id.strip_prefix(BACKLOG_PREFIX) {
        let (repo, issue) = rest.rsplit_once('#').ok_or(LineageError::InvalidBacklog)?;
        let issue: u64 = issue.parse().map_err(|_| LineageError::InvalidBacklog)?;
        return Ok(Some(Lineage::root_child(RootId::backlog(repo, issue)?)));
    }
    let parent = TaskId(parent_id.to_string());
    match lookup(&parent) {
        Some(parent_lineage) => {
            Lineage::child_of(SessionId::from_task(parent), &parent_lineage).map(Some)
        }
        None => Ok(None),
    }
}

/// Combine a lineage already carried by a queue entry with the one derived
/// from its audit proof. They must agree.
pub(crate) fn reconcile(
    carried: Option<Lineage>,
    derived: Option<Lineage>,
) -> Result<Option<Lineage>, LineageError> {
    match (carried, derived) {
        (None, derived) => Ok(derived),
        (Some(carried), Some(derived)) if carried == derived => Ok(Some(carried)),
        _ => Err(LineageError::Inconsistent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tid(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    fn mcp_root() -> Lineage {
        Lineage::root_child(RootId::mcp(Uuid::nil()))
    }

    fn chain(len: usize) -> Result<Lineage, LineageError> {
        let mut lineage = mcp_root();
        for i in 0..len {
            lineage = Lineage::child_of(SessionId::from_task(tid(&format!("s{i}"))), &lineage)?;
        }
        Ok(lineage)
    }

    fn none(_: &TaskId) -> Option<Lineage> {
        None
    }

    #[test]
    fn root_child_is_depth_one_with_no_parent() {
        let lineage = mcp_root();
        assert_eq!(lineage.depth(), 1);
        assert_eq!(lineage.parent(), None);
        assert_eq!(lineage.root(), &RootId::mcp(Uuid::nil()));
    }

    #[test]
    fn child_of_adds_one_to_depth_and_keeps_the_root() {
        let parent = mcp_root();
        let child = Lineage::child_of(SessionId::from_task(tid("p")), &parent).unwrap();
        assert_eq!(child.depth(), 2);
        assert_eq!(child.root(), parent.root());
        assert_eq!(child.parent(), Some(&SessionId::from_task(tid("p"))));
    }

    #[test]
    fn depth_is_capped_at_max_depth() {
        let deepest = chain(usize::from(MAX_DEPTH) - 1).unwrap();
        assert_eq!(deepest.depth(), MAX_DEPTH);
        assert_eq!(
            Lineage::child_of(SessionId::from_task(tid("x")), &deepest),
            Err(LineageError::TooDeep(MAX_DEPTH))
        );
    }

    proptest! {
        #[test]
        fn depth_equals_chain_length_plus_one_until_the_cap(len in 0usize..24) {
            match chain(len) {
                Ok(lineage) => {
                    prop_assert!(len < usize::from(MAX_DEPTH));
                    prop_assert_eq!(usize::from(lineage.depth()), len + 1);
                    prop_assert_eq!(lineage.parent().is_none(), len == 0);
                }
                Err(e) => {
                    prop_assert!(len >= usize::from(MAX_DEPTH));
                    prop_assert_eq!(e, LineageError::TooDeep(MAX_DEPTH));
                }
            }
        }

        #[test]
        fn plan_accepts_exactly_the_documented_alphabet(id in "[ -~\u{e9}\n]{0,100}") {
            let expected = !id.is_empty()
                && id.len() <= MAX_PLAN_ID_LEN
                && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            prop_assert_eq!(RootId::plan(id).is_ok(), expected);
        }

        #[test]
        fn serialized_lineage_roundtrips(len in 0usize..usize::from(MAX_DEPTH)) {
            let lineage = chain(len).unwrap();
            let json = serde_json::to_string(&lineage).unwrap();
            prop_assert_eq!(serde_json::from_str::<Lineage>(&json).unwrap(), lineage);
        }

        #[test]
        fn no_depth_parent_pair_other_than_the_consistent_one_deserializes(depth in 0u8..=255, has_parent in any::<bool>()) {
            let parent = if has_parent { r#","parent":"p""# } else { r#","parent":null"# };
            let json = format!(r#"{{"root":{{"kind":"mcp","id":"00000000-0000-0000-0000-000000000000"}}{parent},"depth":{depth}}}"#);
            let ok = serde_json::from_str::<Lineage>(&json).is_ok();
            let consistent = (depth == 1 && !has_parent)
                || ((2..=MAX_DEPTH).contains(&depth) && has_parent);
            prop_assert_eq!(ok, consistent);
        }
    }

    #[test]
    fn plan_ids_are_validated() {
        assert!(RootId::plan("plan-0a1b-2c").is_ok());
        assert!(RootId::plan("a".repeat(MAX_PLAN_ID_LEN)).is_ok());
        for bad in [
            "",
            "Plan-1",
            "plan 1",
            "plan/1",
            "plan-1\n",
            "plan_1",
            "plan-\u{e9}",
            &"a".repeat(MAX_PLAN_ID_LEN + 1),
        ] {
            assert_eq!(
                RootId::plan(bad),
                Err(LineageError::InvalidPlanId),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn backlog_roots_are_validated() {
        assert!(RootId::backlog("example/repo", 1).is_ok());
        assert!(RootId::backlog("a-b/c_d.e", u64::MAX).is_ok());
        for bad in [
            "",
            "repo",
            "/repo",
            "owner/",
            "a/b/c",
            "../repo",
            "owner/..",
            "o wner/r",
            "o/r\n",
            "o/r#1",
            "o/r\u{e9}",
        ] {
            assert_eq!(
                RootId::backlog(bad, 1),
                Err(LineageError::InvalidBacklog),
                "{bad:?}"
            );
        }
        let long = format!("o/{}", "r".repeat(200));
        assert_eq!(RootId::backlog(long, 1), Err(LineageError::InvalidBacklog));
    }

    #[test]
    fn every_root_kind_roundtrips_with_a_stable_wire_shape() {
        let nil = Uuid::nil();
        let cases = [
            (
                RootId::mcp(nil),
                r#"{"kind":"mcp","id":"00000000-0000-0000-0000-000000000000"}"#,
            ),
            (
                RootId::plan("plan-1").unwrap(),
                r#"{"kind":"plan","id":"plan-1"}"#,
            ),
            (
                RootId::backlog("o/r", 3).unwrap(),
                r#"{"kind":"backlog","repo":"o/r","issue":3}"#,
            ),
            (RootId::legacy(tid("t")), r#"{"kind":"legacy","task":"t"}"#),
        ];
        for (root, json) in cases {
            assert_eq!(serde_json::to_string(&root).unwrap(), json);
            assert_eq!(serde_json::from_str::<RootId>(json).unwrap(), root);
        }
    }

    #[test]
    fn forged_roots_do_not_deserialize() {
        for json in [
            r#"{"kind":"plan","id":"PLAN"}"#,
            r#"{"kind":"plan","id":""}"#,
            r#"{"kind":"backlog","repo":"../x","issue":1}"#,
            r#"{"kind":"backlog","repo":"a/b/c","issue":1}"#,
            r#"{"kind":"mcp","id":"not-a-uuid"}"#,
            r#"{"kind":"nope"}"#,
        ] {
            assert!(serde_json::from_str::<RootId>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn forged_lineages_do_not_deserialize() {
        let root = r#"{"kind":"mcp","id":"00000000-0000-0000-0000-000000000000"}"#;
        for (parent, depth) in [
            ("null", 5),
            ("null", 0),
            (r#""p""#, 1),
            (r#""p""#, 0),
            (r#""p""#, 9),
            (r#""p""#, 255),
        ] {
            let json = format!(r#"{{"root":{root},"parent":{parent},"depth":{depth}}}"#);
            let err = serde_json::from_str::<Lineage>(&json)
                .unwrap_err()
                .to_string();
            assert!(err.contains("depth and parent disagree"), "{json}: {err}");
        }
        let forged_plan = r#"{"root":{"kind":"plan","id":"A B"},"parent":null,"depth":1}"#;
        assert!(serde_json::from_str::<Lineage>(forged_plan).is_err());
    }

    #[test]
    fn task_id_returns_the_task_the_session_was_built_from() {
        let first = SessionId::from_task(tid("task-one"));
        let second = SessionId::from_task(tid("task-two"));
        assert_eq!(first.task_id(), &tid("task-one"));
        assert_eq!(second.task_id(), &tid("task-two"));
        assert_ne!(first.task_id(), second.task_id());
        assert_eq!(first.task_id().0, first.to_string());
        assert_eq!(second.task_id().0, second.to_string());
        assert_ne!(first.to_string(), second.to_string());
    }

    #[test]
    fn session_id_serializes_as_the_task_id_string() {
        let session = SessionId::from_task(tid("abc"));
        assert_eq!(serde_json::to_string(&session).unwrap(), r#""abc""#);
        assert_eq!(session.to_string(), "abc");
        assert!(SessionId::from_task(tid("a")) < SessionId::from_task(tid("b")));
    }

    #[test]
    fn derive_maps_an_mcp_parent_to_an_mcp_root() {
        let id = Uuid::new_v4();
        let derived = derive_lineage(&format!("mcp-{id}"), &none).unwrap();
        assert_eq!(derived, Some(Lineage::root_child(RootId::mcp(id))));
    }

    #[test]
    fn derive_refuses_malformed_mcp_ids() {
        let simple = Uuid::new_v4().simple().to_string();
        for bad in [
            "mcp-".to_string(),
            "mcp-nope".to_string(),
            format!("mcp-{simple}"),
            format!("mcp-{}", Uuid::new_v4().to_string().to_uppercase() + "x"),
            format!("mcp-{{{}}}", Uuid::new_v4()),
        ] {
            assert_eq!(
                derive_lineage(&bad, &none),
                Err(LineageError::InvalidRoot),
                "{bad}"
            );
        }
    }

    #[test]
    fn derive_maps_plan_and_backlog_parents() {
        assert_eq!(
            derive_lineage("plan-0a1b", &none).unwrap(),
            Some(Lineage::root_child(RootId::plan("plan-0a1b").unwrap()))
        );
        assert_eq!(
            derive_lineage("backlog-example/repo#42", &none).unwrap(),
            Some(Lineage::root_child(
                RootId::backlog("example/repo", 42).unwrap()
            ))
        );
    }

    #[test]
    fn derive_refuses_malformed_plan_and_backlog_ids() {
        assert_eq!(
            derive_lineage("plan-BAD", &none),
            Err(LineageError::InvalidPlanId)
        );
        for bad in [
            "backlog-",
            "backlog-example/repo",
            "backlog-example/repo#",
            "backlog-example/repo#-1",
            "backlog-example/repo#x",
            "backlog-../x#1",
            "backlog-a/b/c#1",
        ] {
            assert_eq!(
                derive_lineage(bad, &none),
                Err(LineageError::InvalidBacklog),
                "{bad}"
            );
        }
    }

    #[test]
    fn derive_maps_a_known_task_to_a_child() {
        let parent_lineage = Lineage::root_child(RootId::plan("plan-9").unwrap());
        let lookup = |id: &TaskId| (id.0 == "known").then(|| parent_lineage.clone());
        let child = derive_lineage("known", &lookup).unwrap().unwrap();
        assert_eq!(child.depth(), 2);
        assert_eq!(child.parent(), Some(&SessionId::from_task(tid("known"))));
        assert_eq!(child.root(), parent_lineage.root());
    }

    #[test]
    fn derive_propagates_too_deep_from_a_known_parent() {
        let deepest = chain(usize::from(MAX_DEPTH) - 1).unwrap();
        let lookup = |_: &TaskId| Some(deepest.clone());
        assert_eq!(
            derive_lineage("known", &lookup),
            Err(LineageError::TooDeep(MAX_DEPTH))
        );
    }

    #[test]
    fn derive_leaves_unknown_and_unrecognised_parents_without_a_lineage() {
        let cli = format!("cli-{}", Uuid::new_v4());
        for id in ["task-1", "parent-1", "restore-abc", "t", "", cli.as_str()] {
            assert_eq!(derive_lineage(id, &none), Ok(None), "{id}");
        }
    }

    #[test]
    fn reconcile_requires_agreement() {
        let a = Lineage::root_child(RootId::plan("plan-a").unwrap());
        let b = Lineage::root_child(RootId::plan("plan-b").unwrap());
        assert_eq!(reconcile(None, None), Ok(None));
        assert_eq!(reconcile(None, Some(a.clone())), Ok(Some(a.clone())));
        assert_eq!(
            reconcile(Some(a.clone()), Some(a.clone())),
            Ok(Some(a.clone()))
        );
        assert_eq!(
            reconcile(Some(a.clone()), Some(b)),
            Err(LineageError::Inconsistent)
        );
        assert_eq!(reconcile(Some(a), None), Err(LineageError::Inconsistent));
    }

    #[test]
    fn hub_sources_cannot_reach_the_audit_tokens() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hub");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let production = text.split("#[cfg(test)]").next().unwrap();
                for token in ["AuditedIdentity", "BoundIdentity", "into_parts"] {
                    assert!(
                        !production.contains(token),
                        "{} mentions {token}",
                        path.display()
                    );
                }
                scanned += 1;
            }
        }
        assert!(scanned >= 2);
    }
}
