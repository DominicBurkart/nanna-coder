use super::lineage::{RootId, SessionId};
use super::message::MessageId;
use super::store::{MessageStore, SessionStore};
use crate::action_auditor::IdentifierOracle;
use crate::task::TaskId;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Asks whether a git object exists in a repository directory.
pub trait GitRunner: Send + Sync {
    fn object_exists(&self, dir: &Path, hex: &str) -> bool;
}

/// Runs `git -C <dir> cat-file -e <hex>` with a fixed argument vector, no shell and an
/// environment scrubbed of `GIT_*` variables.
///
/// ```
/// use harness::hub::oracle::{GitRunner, SystemGit};
/// let dir = tempfile::tempdir().unwrap();
/// assert!(!SystemGit.object_exists(dir.path(), "0123456789abcdef0123456789abcdef01234567"));
/// ```
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn object_exists(&self, dir: &Path, hex: &str) -> bool {
        let mut git = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                git.env_remove(&name);
            }
        }
        git.arg("-C").arg(dir).args(["cat-file", "-e", hex]);
        git.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        git.status().is_ok_and(|status| status.success())
    }
}

fn is_object_name(hex: &str) -> bool {
    (4..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Vouches for a hex token only when the object exists in the sender's own workspace. The
/// directory is fixed by the host when the oracle is built; nothing the sender writes selects it.
///
/// ```
/// use harness::action_auditor::IdentifierOracle;
/// use harness::hub::oracle::{GitObjectOracle, SystemGit};
/// use std::sync::Arc;
///
/// let oracle = GitObjectOracle::new("/nonexistent-workspace".into(), Arc::new(SystemGit));
/// assert!(!oracle.is_known_git_object("not hex"));
/// assert!(!oracle.is_known_hub_id("123e4567-e89b-42d3-a456-426614174000"));
/// ```
pub struct GitObjectOracle {
    dir: PathBuf,
    runner: Arc<dyn GitRunner>,
}

impl GitObjectOracle {
    pub fn new(dir: PathBuf, runner: Arc<dyn GitRunner>) -> Self {
        Self { dir, runner }
    }
}

impl IdentifierOracle for GitObjectOracle {
    fn is_known_git_object(&self, hex: &str) -> bool {
        is_object_name(hex) && self.runner.object_exists(&self.dir, hex)
    }

    fn is_known_hub_id(&self, _id: &str) -> bool {
        false
    }
}

/// Vouches for a UUID-shaped token only when it is a session or message id of the sender's own
/// root, so a guess at an id of another root is scored like any other token.
///
/// ```
/// use harness::action_auditor::IdentifierOracle;
/// use harness::hub::lineage::RootId;
/// use harness::hub::oracle::HubIdOracle;
/// use harness::hub::store::{InMemoryMessageStore, InMemorySessionStore};
/// use std::sync::Arc;
///
/// let root = RootId::mcp(uuid::Uuid::nil());
/// let oracle = HubIdOracle::new(root, Arc::new(InMemorySessionStore::new()), Arc::new(InMemoryMessageStore::new()));
/// assert!(!oracle.is_known_hub_id("123e4567-e89b-42d3-a456-426614174000"));
/// assert!(!oracle.is_known_git_object("0123456789abcdef0123456789abcdef01234567"));
/// ```
pub struct HubIdOracle {
    root: RootId,
    sessions: Arc<dyn SessionStore>,
    messages: Arc<dyn MessageStore>,
}

impl HubIdOracle {
    pub fn new(
        root: RootId,
        sessions: Arc<dyn SessionStore>,
        messages: Arc<dyn MessageStore>,
    ) -> Self {
        Self {
            root,
            sessions,
            messages,
        }
    }
}

impl IdentifierOracle for HubIdOracle {
    fn is_known_git_object(&self, _hex: &str) -> bool {
        false
    }

    fn is_known_hub_id(&self, id: &str) -> bool {
        let session = SessionId::from_task(TaskId(id.to_string()));
        let own_session =
            matches!(self.sessions.get(session), Ok(Some(rec)) if rec.lineage.root() == &self.root);
        let message = uuid::Uuid::parse_str(id).map(MessageId::from_uuid);
        let own_message = matches!(message.map(|m| self.messages.get(m)), Ok(Ok(Some(msg))) if msg.root == self.root);
        own_session || own_message
    }
}

/// Git questions go to `git`, hub-id questions to `ids`.
///
/// ```
/// use harness::action_auditor::{IdentifierOracle, NullOracle};
/// use harness::hub::oracle::ComposedOracle;
/// use std::sync::Arc;
///
/// let oracle = ComposedOracle::new(Arc::new(NullOracle), Arc::new(NullOracle));
/// assert!(!oracle.is_known_git_object("abcd"));
/// ```
pub struct ComposedOracle {
    git: Arc<dyn IdentifierOracle + Send + Sync>,
    ids: Arc<dyn IdentifierOracle + Send + Sync>,
}

impl ComposedOracle {
    pub fn new(
        git: Arc<dyn IdentifierOracle + Send + Sync>,
        ids: Arc<dyn IdentifierOracle + Send + Sync>,
    ) -> Self {
        Self { git, ids }
    }
}

impl IdentifierOracle for ComposedOracle {
    fn is_known_git_object(&self, hex: &str) -> bool {
        self.git.is_known_git_object(hex)
    }

    fn is_known_hub_id(&self, id: &str) -> bool {
        self.ids.is_known_hub_id(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_auditor::NullOracle;
    use crate::hub::lineage::Lineage;
    use crate::hub::message::{DeliveryState, MessageKind, Seq, StoredMessage, VerdictRef};
    use crate::hub::registry::SessionRegistry;
    use crate::hub::store::{InMemoryMessageStore, InMemorySessionStore};
    use std::sync::Mutex;
    use uuid::Uuid;

    struct Recording {
        calls: Mutex<Vec<(PathBuf, String)>>,
        answer: bool,
    }

    impl GitRunner for Recording {
        fn object_exists(&self, dir: &Path, hex: &str) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push((dir.to_path_buf(), hex.to_string()));
            self.answer
        }
    }

    #[test]
    fn the_git_oracle_asks_the_fixed_directory_and_only_about_hex_names() {
        let runner = Arc::new(Recording {
            calls: Mutex::new(Vec::new()),
            answer: true,
        });
        let oracle = GitObjectOracle::new(PathBuf::from("/ws/own"), runner.clone());
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert!(oracle.is_known_git_object(sha));
        for bad in [
            "",
            "abc",
            "-x",
            "--upload-pack=x",
            "0123456789abcdef; rm",
            "../../etc",
            &"a".repeat(65),
        ] {
            assert!(!oracle.is_known_git_object(bad), "{bad}");
        }
        assert!(!oracle.is_known_hub_id(sha));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(&*calls, &[(PathBuf::from("/ws/own"), sha.to_string())]);
    }

    #[test]
    fn a_negative_answer_from_git_is_not_a_vouch() {
        let runner = Arc::new(Recording {
            calls: Mutex::new(Vec::new()),
            answer: false,
        });
        let oracle = GitObjectOracle::new(PathBuf::from("/ws"), runner);
        assert!(!oracle.is_known_git_object("deadbeef"));
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        let mut command = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.status().is_ok_and(|status| status.success())
    }

    #[test]
    fn the_system_runner_sees_a_real_commit_and_not_a_made_up_object() {
        let dir = tempfile::tempdir().unwrap();
        assert!(git(dir.path(), &["init", "-q"]));
        assert!(git(
            dir.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x"
            ]
        ));
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "HEAD"])
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        let head = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_eq!(head.len(), 40);
        assert!(SystemGit.object_exists(dir.path(), &head));
        assert!(!SystemGit.object_exists(dir.path(), "0123456789abcdef0123456789abcdef01234567"));
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(!SystemGit.object_exists(elsewhere.path(), &head));
    }

    fn message(id: MessageId, root: &RootId) -> StoredMessage {
        let from = SessionId::from_task(TaskId("a".into()));
        let to = SessionId::from_task(TaskId("b".into()));
        let verdict = VerdictRef {
            log_id: "v".into(),
            content_hash: 1,
        };
        StoredMessage {
            id,
            seq: Seq::new(0),
            from,
            from_identity: "x".into(),
            to,
            root: root.clone(),
            kind: MessageKind::Info,
            subject: "s".into(),
            body: "b".into(),
            in_reply_to: None,
            thread_depth: 0,
            outbound_verdict: verdict,
            state: DeliveryState::Pending,
            sent_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn the_hub_id_oracle_vouches_only_for_ids_of_the_senders_own_root() {
        let own = RootId::mcp(Uuid::from_u128(1));
        let foreign = RootId::mcp(Uuid::from_u128(2));
        let sessions = Arc::new(InMemorySessionStore::new());
        let messages = Arc::new(InMemoryMessageStore::new());
        let registry = SessionRegistry::new(sessions.clone());
        let card = crate::identity::example();
        let (own_id, foreign_id) = (
            Uuid::from_u128(11).to_string(),
            Uuid::from_u128(12).to_string(),
        );
        for (name, root) in [(&own_id, &own), (&foreign_id, &foreign)] {
            let binding = crate::hub::lineage::SessionBinding::from_audited(
                &card,
                TaskId(name.clone()),
                Lineage::root_child(root.clone()),
            );
            registry
                .register(&binding, "t", chrono::Utc::now())
                .unwrap();
        }
        let (own_msg, foreign_msg) = (MessageId::new(), MessageId::new());
        messages.append(message(own_msg, &own)).unwrap();
        messages.append(message(foreign_msg, &foreign)).unwrap();
        let oracle = HubIdOracle::new(own, sessions, messages);
        assert!(
            oracle.is_known_hub_id(&own_id)
                && oracle.is_known_hub_id(&own_msg.as_uuid().to_string())
        );
        assert!(
            !oracle.is_known_hub_id(&foreign_id)
                && !oracle.is_known_hub_id(&foreign_msg.as_uuid().to_string())
        );
        assert!(
            !oracle.is_known_hub_id(&Uuid::from_u128(99).to_string())
                && !oracle.is_known_hub_id("not a uuid")
        );
        assert!(!oracle.is_known_git_object("0123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn the_composed_oracle_routes_each_question_to_its_own_source() {
        struct Yes;
        impl IdentifierOracle for Yes {
            fn is_known_git_object(&self, _hex: &str) -> bool {
                true
            }
            fn is_known_hub_id(&self, _id: &str) -> bool {
                true
            }
        }
        let git_only = ComposedOracle::new(Arc::new(Yes), Arc::new(NullOracle));
        assert!(git_only.is_known_git_object("abcd") && !git_only.is_known_hub_id("x"));
        let ids_only = ComposedOracle::new(Arc::new(NullOracle), Arc::new(Yes));
        assert!(!ids_only.is_known_git_object("abcd") && ids_only.is_known_hub_id("x"));
    }
}
