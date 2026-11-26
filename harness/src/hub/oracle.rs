use super::lineage::{RootId, SessionId};
use super::message::MessageId;
use super::store::{MessageStore, SessionStore};
use crate::action_auditor::IdentifierOracle;
use crate::task::TaskId;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn git_env_names(vars: impl Iterator<Item = OsString>) -> Vec<OsString> {
    vars.filter(|name| name.to_string_lossy().starts_with("GIT_"))
        .collect()
}

fn process_git_env_names() -> Vec<OsString> {
    git_env_names(std::env::vars_os().map(|(name, _)| name))
}

fn scrub_git_env(command: &mut Command, names: impl IntoIterator<Item = OsString>) {
    for name in names {
        command.env_remove(name);
    }
}

/// How long one `git cat-file` may run before it is killed and the token is not vouched.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How many `git` processes one oracle may start; later tokens are not vouched.
pub const MAX_GIT_LOOKUPS: usize = 8;

fn git_command(dir: &Path, hex: &str, names: Vec<OsString>) -> Command {
    let mut git = Command::new("git");
    scrub_git_env(&mut git, names);
    git.arg("-C").arg(dir).args(["cat-file", "-e", hex]);
    git.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    git
}

fn run_bounded(mut command: Command, limit: Duration) -> bool {
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

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
        run_bounded(git_command(dir, hex, process_git_env_names()), GIT_TIMEOUT)
    }
}

fn is_object_name(hex: &str) -> bool {
    (4..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Vouches for a hex token only when the object exists in the sender's own workspace. Build one oracle per review: it starts at most [`MAX_GIT_LOOKUPS`] git processes, each bounded by [`GIT_TIMEOUT`]. The
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
    lookups: AtomicUsize,
}

impl GitObjectOracle {
    pub fn new(dir: PathBuf, runner: Arc<dyn GitRunner>) -> Self {
        Self {
            dir,
            runner,
            lookups: AtomicUsize::new(0),
        }
    }
}

impl IdentifierOracle for GitObjectOracle {
    fn is_known_git_object(&self, hex: &str) -> bool {
        is_object_name(hex)
            && self.lookups.fetch_add(1, Ordering::SeqCst) < MAX_GIT_LOOKUPS
            && self.runner.object_exists(&self.dir, hex)
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

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_names_with_the_git_prefix_are_selected_for_scrubbing() {
        let all = names(&[
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_",
            "GITHUB_TOKEN",
            "PATH",
            "",
            "xGIT_DIR",
            "git_dir",
        ]);
        let picked = git_env_names(all.into_iter());
        assert_eq!(picked, names(&["GIT_DIR", "GIT_INDEX_FILE", "GIT_"]));
        assert!(git_env_names(std::iter::empty()).is_empty());
    }

    #[test]
    fn scrubbing_marks_exactly_the_given_names_removed() {
        let mut command = Command::new("git");
        command.env("KEEP", "1");
        scrub_git_env(&mut command, names(&["GIT_DIR", "GIT_INDEX_FILE"]));
        let envs: Vec<(OsString, Option<OsString>)> = command
            .get_envs()
            .map(|(name, value)| (name.to_os_string(), value.map(OsString::from)))
            .collect();
        assert!(envs.contains(&(OsString::from("GIT_DIR"), None)));
        assert!(envs.contains(&(OsString::from("GIT_INDEX_FILE"), None)));
        assert!(envs.contains(&(OsString::from("KEEP"), Some(OsString::from("1")))));
        assert_eq!(envs.len(), 3);
        let mut untouched = Command::new("git");
        scrub_git_env(&mut untouched, Vec::new());
        assert_eq!(untouched.get_envs().count(), 0);
    }

    #[test]
    fn the_process_names_helper_selects_only_git_prefixed_names() {
        assert!(process_git_env_names()
            .iter()
            .all(|name| name.to_string_lossy().starts_with("GIT_")));
    }

    #[test]
    fn the_real_command_is_git_with_the_fixed_arguments_and_the_given_names_removed() {
        let hex = "0123456789abcdef";
        let command = git_command(
            Path::new("/ws/own"),
            hex,
            names(&["GIT_DIR", "GIT_WORK_TREE"]),
        );
        assert_eq!(command.get_program(), "git");
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(args, ["-C", "/ws/own", "cat-file", "-e", hex]);
        let envs: Vec<(OsString, Option<OsString>)> = command
            .get_envs()
            .map(|(name, value)| (name.to_os_string(), value.map(OsString::from)))
            .collect();
        assert_eq!(envs.len(), 2);
        assert!(envs.contains(&(OsString::from("GIT_DIR"), None)));
        assert!(envs.contains(&(OsString::from("GIT_WORK_TREE"), None)));
        assert_eq!(command.get_current_dir(), None);
    }

    #[test]
    fn a_bounded_run_reports_success_failure_and_a_missing_program() {
        let mut version = Command::new("git");
        version.arg("--version").stdout(Stdio::null());
        assert!(run_bounded(version, GIT_TIMEOUT));
        let mut unknown = Command::new("git");
        unknown
            .args(["no-such-subcommand-xyz"])
            .stderr(Stdio::null());
        assert!(!run_bounded(unknown, GIT_TIMEOUT));
        assert!(!run_bounded(
            Command::new("no-such-program-xyz"),
            GIT_TIMEOUT
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_run_that_outlasts_the_limit_is_killed_and_not_vouched() {
        let mut slow = Command::new("sleep");
        slow.arg("30");
        let started = Instant::now();
        assert!(!run_bounded(slow, Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn only_the_first_eight_valid_lookups_start_git_and_invalid_names_cost_nothing() {
        let runner = Arc::new(Recording {
            calls: Mutex::new(Vec::new()),
            answer: true,
        });
        let oracle = GitObjectOracle::new(PathBuf::from("/ws"), runner.clone());
        for bad in ["zz", "", "not hex", "--x"] {
            assert!(!oracle.is_known_git_object(bad));
        }
        let vouched: Vec<bool> = (0..12)
            .map(|n| oracle.is_known_git_object(&format!("{:040x}", n + 1)))
            .collect();
        assert_eq!(vouched.iter().filter(|v| **v).count(), MAX_GIT_LOOKUPS);
        assert!(vouched[..MAX_GIT_LOOKUPS].iter().all(|v| *v));
        assert!(vouched[MAX_GIT_LOOKUPS..].iter().all(|v| !*v));
        assert_eq!(runner.calls.lock().unwrap().len(), MAX_GIT_LOOKUPS);
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
        scrub_git_env(&mut command, process_git_env_names());
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
            held: Vec::new(),
            released: None,
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
