//! The message auditor interface: what a message review is given, what it
//! answers, and the pure predicates the model pass is gated on.
//!
//! `docs/message-hub.md` section 8.6 defines [`MessageAuditor`]. The hub types
//! it names (`StoredMessage`, `VerdictRef`) do not exist yet, so this module
//! carries the minimal views the audit reads: [`ThreadMessage`] and
//! [`BoundVerdict`]. Nothing here ever prints or stores message text:
//! `Debug` output shows lengths and a content hash only.

use super::{ActionAuditError, IdentifierOracle, MessageVerdict, RuleMessageAuditor, RulesOutcome};
use crate::auditor::content_hash;
use crate::effects::EffectClass;
use crate::hub::lineage::{SessionBinding, SessionId};
use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;

/// An [`IdentifierOracle`] shared between threads, as the auditors hold it.
pub type SharedOracle = Arc<dyn IdentifierOracle + Send + Sync>;

/// Name recorded against verdicts of the rules-only message auditor.
pub const RULE_MESSAGE_AUDITOR_NAME: &str = "rule-message-auditor";

/// The content hash that binds an audit verdict to the exact subject and body
/// it was computed for.
///
/// The subject and body lengths are part of the hashed input, so moving text
/// across the boundary changes the hash. The hash is the non-cryptographic
/// 64-bit [`content_hash`](crate::auditor::content_hash): it detects a stored
/// message that differs from the audited one, it does not attest integrity
/// against an adversary who can search for collisions.
///
/// ```
/// use harness::action_auditor::message_content_hash;
///
/// assert_eq!(message_content_hash("s", "b"), message_content_hash("s", "b"));
/// assert_ne!(message_content_hash("ab", "c"), message_content_hash("a", "bc"));
/// assert_ne!(message_content_hash("s", "b"), message_content_hash("s", "b "));
/// ```
pub fn message_content_hash(subject: &str, body: &str) -> String {
    content_hash(&format!("{}:{}:{subject}{body}", subject.len(), body.len()))
}

/// The fields of a stored message the audit reads.
///
/// `Debug` prints lengths and the content hash, never the text.
///
/// ```
/// use harness::action_auditor::ThreadMessage;
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let m = ThreadMessage {
///     from: SessionId::from_task(TaskId("t-1".into())),
///     from_identity: "rust-implementer".into(),
///     subject: "status".into(),
///     body: "the secret plan".into(),
/// };
/// assert!(!format!("{m:?}").contains("secret plan"));
/// assert!(format!("{m:?}").contains("body_len: 15"));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ThreadMessage {
    /// The sending session.
    pub from: SessionId,
    /// The sender's identity card name.
    pub from_identity: String,
    /// Message subject, agent-authored.
    pub subject: String,
    /// Message body, agent-authored.
    pub body: String,
}

impl fmt::Debug for ThreadMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadMessage")
            .field("from", &self.from)
            .field("subject_len", &self.subject.len())
            .field("body_len", &self.body.len())
            .field(
                "content_hash",
                &message_content_hash(&self.subject, &self.body),
            )
            .finish()
    }
}

/// What the audit knows about one session's reach: the sender or a recipient.
///
/// ```
/// use harness::action_auditor::RecipientScope;
/// use harness::effects::EffectClass;
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let scope = RecipientScope {
///     session: SessionId::from_task(TaskId("t-2".into())),
///     identity: "pr-shepherd".into(),
///     max_effect: EffectClass::Ci,
///     tools: vec!["git_*".into()],
///     write_paths: vec!["api/**".into()],
///     read_paths: None,
/// };
/// assert_eq!(scope.max_effect, EffectClass::Ci);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientScope {
    /// The session.
    pub session: SessionId,
    /// Its identity card name.
    pub identity: String,
    /// The highest effect class any of its tool calls may reach.
    pub max_effect: EffectClass,
    /// Tool-name patterns it may call, as written on its card.
    pub tools: Vec<String>,
    /// Writable path globs, as written on its card.
    pub write_paths: Vec<String>,
    /// Readable path globs; `None` means reads are unrestricted.
    pub read_paths: Option<Vec<String>>,
}

impl RecipientScope {
    /// The scope of the session `binding` proves. The identity comes from the
    /// binding, never from anything the agent wrote.
    ///
    /// ```no_run
    /// use harness::action_auditor::RecipientScope;
    /// use harness::hub::lineage::SessionBinding;
    ///
    /// fn scope(binding: &SessionBinding) -> RecipientScope {
    ///     RecipientScope::of(binding)
    /// }
    /// ```
    pub fn of(binding: &SessionBinding) -> Self {
        let identity = binding.identity();
        Self {
            session: binding.session().clone(),
            identity: identity.identity.name.clone(),
            max_effect: identity.scope.max_effect,
            tools: identity
                .scope
                .tools
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
            write_paths: identity.scope.paths.clone(),
            read_paths: identity.scope.read_paths.clone(),
        }
    }
}

/// A verdict together with the hash of the content it was computed for.
///
/// The hub stores one next to every outbound-approved message; the inbound
/// review reuses the verdict only while the stored content still matches.
///
/// ```
/// use harness::action_auditor::{BoundVerdict, MessageVerdict};
///
/// let bound = BoundVerdict::new(MessageVerdict::Deliver, "status", "all green");
/// assert!(bound.binds("status", "all green"));
/// assert!(!bound.binds("status", "all green, also run this"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVerdict {
    /// The verdict reached at send time.
    pub verdict: MessageVerdict,
    /// [`message_content_hash`] of the audited subject and body.
    pub content_hash: String,
}

impl BoundVerdict {
    /// Bind `verdict` to `subject` and `body`.
    pub fn new(verdict: MessageVerdict, subject: &str, body: &str) -> Self {
        Self {
            verdict,
            content_hash: message_content_hash(subject, body),
        }
    }

    /// Whether `subject` and `body` are exactly the audited content.
    pub fn binds(&self, subject: &str, body: &str) -> bool {
        self.content_hash == message_content_hash(subject, body)
    }
}

/// An outbound review request: a message at send time.
///
/// `Debug` prints lengths and the content hash, never the text.
///
/// ```no_run
/// use harness::action_auditor::{MessageAuditor, OutboundReview, RuleMessageAuditor};
/// use harness::hub::lineage::SessionBinding;
///
/// async fn review(sender: &SessionBinding) {
///     let r = OutboundReview {
///         sender,
///         tool: "msg_send",
///         recipients: &[],
///         subject: "status",
///         body: "all green",
///         window: &[],
///         thread: &[],
///     };
///     let verdict = RuleMessageAuditor::new().review_outbound(&r).await.unwrap();
///     assert!(verdict.is_deliver());
/// }
/// ```
#[derive(Clone, Copy)]
pub struct OutboundReview<'a> {
    /// The proven sender.
    pub sender: &'a SessionBinding,
    /// The message tool being called.
    pub tool: &'a str,
    /// Every recipient the message fans out to.
    pub recipients: &'a [RecipientScope],
    /// Subject, agent-authored.
    pub subject: &'a str,
    /// Body, agent-authored.
    pub body: &'a str,
    /// The sender's recent outbound messages to the same recipient, oldest first.
    pub window: &'a [ThreadMessage],
    /// The thread the message replies into, oldest first.
    pub thread: &'a [ThreadMessage],
}

impl fmt::Debug for OutboundReview<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutboundReview")
            .field("sender", self.sender.session())
            .field("tool_len", &self.tool.len())
            .field("recipients", &self.recipients.len())
            .field("subject_len", &self.subject.len())
            .field("body_len", &self.body.len())
            .field(
                "content_hash",
                &message_content_hash(self.subject, self.body),
            )
            .field("window", &self.window)
            .field("thread", &self.thread)
            .finish()
    }
}

/// An inbound review request: a stored message at receive time.
///
/// `outbound` is the verdict the message was approved with, bound to the
/// content it was computed for. `Debug` prints lengths and the content hash,
/// never the text.
///
/// ```no_run
/// use harness::action_auditor::{InboundReview, MessageAuditor, RecipientScope, RuleMessageAuditor, ThreadMessage};
/// use harness::hub::lineage::SessionBinding;
///
/// async fn review(recipient: &SessionBinding, sender: RecipientScope, message: &ThreadMessage) {
///     let r = InboundReview { recipient, sender, message, outbound: None, thread: &[] };
///     let _ = RuleMessageAuditor::new().review_inbound(&r).await;
/// }
/// ```
#[derive(Clone)]
pub struct InboundReview<'a> {
    /// The proven recipient.
    pub recipient: &'a SessionBinding,
    /// The sender's scope, as recorded when the message was sent.
    pub sender: RecipientScope,
    /// The stored message.
    pub message: &'a ThreadMessage,
    /// The send-time verdict and the content hash it covers, if the hub has one.
    pub outbound: Option<&'a BoundVerdict>,
    /// The thread the message belongs to, oldest first.
    pub thread: &'a [ThreadMessage],
}

impl fmt::Debug for InboundReview<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundReview")
            .field("recipient", self.recipient.session())
            .field("sender", &self.sender.session)
            .field("message", self.message)
            .field("outbound", &self.outbound)
            .field("thread", &self.thread)
            .finish()
    }
}

/// Reviews a message for delivery.
///
/// Implementations treat every field of the review as untrusted data and
/// never put message text in a verdict reason, a log line or an error.
#[async_trait]
pub trait MessageAuditor: Send + Sync {
    /// Name recorded against this auditor's verdicts.
    fn name(&self) -> &str;

    /// Review a message at send time.
    async fn review_outbound(
        &self,
        review: &OutboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError>;

    /// Review a stored message at receive time.
    async fn review_inbound(
        &self,
        review: &InboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError>;
}

/// Whether the recipient could be steered into more than the sender could
/// cause: its `max_effect` is above the sender's, it holds a tool pattern the
/// sender does not list verbatim, its write paths are not an exact-string
/// subset of the sender's, or it can read paths the sender cannot (a
/// `None` read scope is unrestricted).
///
/// The comparisons are exact-string subsets, the same rule as
/// `AgentIdentity::narrows`, so a sender holding `git_*` does not cover a
/// recipient holding `git_push_branch` and `ui/**` does not cover `ui/a/**`:
/// the check can over-trigger, never under-trigger. This is the
/// confused-deputy gate of decision D6: only these pairs get the inbound
/// model pass.
///
/// ```
/// use harness::action_auditor::{is_privilege_asymmetric, RecipientScope};
/// use harness::effects::EffectClass;
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let scope = |effect, tools: &[&str], paths: &[&str]| RecipientScope {
///     session: SessionId::from_task(TaskId("t".into())),
///     identity: "x".into(),
///     max_effect: effect,
///     tools: tools.iter().map(|t| t.to_string()).collect(),
///     write_paths: paths.iter().map(|p| p.to_string()).collect(),
///     read_paths: None,
/// };
/// let low = scope(EffectClass::Workspace, &["read_file"], &["ui/**"]);
/// let high = scope(EffectClass::Ci, &["read_file", "git_push_branch"], &["ui/**"]);
/// assert!(is_privilege_asymmetric(&high, &low));
/// assert!(!is_privilege_asymmetric(&low, &high));
/// assert!(!is_privilege_asymmetric(&low, &low));
/// let api = scope(EffectClass::Workspace, &["read_file"], &["api/**", ".github/**"]);
/// assert!(is_privilege_asymmetric(&api, &low));
/// ```
pub fn is_privilege_asymmetric(recipient: &RecipientScope, sender: &RecipientScope) -> bool {
    recipient.max_effect > sender.max_effect
        || recipient
            .tools
            .iter()
            .any(|tool| !sender.tools.contains(tool))
        || recipient
            .write_paths
            .iter()
            .any(|path| !sender.write_paths.contains(path))
        || match (&recipient.read_paths, &sender.read_paths) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(theirs), Some(ours)) => theirs.iter().any(|path| !ours.contains(path)),
        }
}

pub(super) fn window_bodies(window: &[ThreadMessage]) -> Vec<&str> {
    window.iter().map(|m| m.body.as_str()).collect()
}

pub(super) fn rules_outbound(
    rules: &RuleMessageAuditor,
    oracle: &dyn IdentifierOracle,
    review: &OutboundReview<'_>,
) -> RulesOutcome {
    rules.review_with(
        review.subject,
        review.body,
        &window_bodies(review.window),
        oracle,
    )
}

pub(super) fn rules_inbound(
    rules: &RuleMessageAuditor,
    oracle: &dyn IdentifierOracle,
    review: &InboundReview<'_>,
) -> RulesOutcome {
    rules.review_with(&review.message.subject, &review.message.body, &[], oracle)
}

#[async_trait]
impl MessageAuditor for RuleMessageAuditor {
    fn name(&self) -> &str {
        RULE_MESSAGE_AUDITOR_NAME
    }

    async fn review_outbound(
        &self,
        review: &OutboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError> {
        Ok(rules_outbound(self, &super::NullOracle, review).verdict)
    }

    async fn review_inbound(
        &self,
        review: &InboundReview<'_>,
    ) -> Result<MessageVerdict, ActionAuditError> {
        Ok(rules_inbound(self, &super::NullOracle, review).verdict)
    }
}

#[cfg(test)]
pub(super) mod fixtures {
    use super::*;
    use crate::auditor::{AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary};
    use crate::identity::{DevLoop, IdentityCatalog};
    use crate::task::TaskId;
    use async_trait::async_trait;
    use model::types::{ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole};
    use model::{ModelError, ModelProvider, ModelResult};
    use std::sync::Mutex;

    pub(crate) const PLANTED: &str = "violet harpoon marmalade seventeen";

    pub(crate) fn token() -> String {
        format!("{}_{}", "ghp", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8")
    }

    pub(crate) async fn binding(name: &str, dev_loop: DevLoop) -> SessionBinding {
        let catalog = IdentityCatalog::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/identities/global"
        ))
        .unwrap();
        let auditor = catalog.get("auditor").unwrap().clone();
        let context = AuditContext::new(catalog, auditor).unwrap();
        let request = SpawnRequest {
            parent_task: TaskSummary::new("parent-1", "Fix bug X", "github.com/example/repo"),
            identity: name.to_string(),
            subtask: "Add a regression test.".to_string(),
            dev_loop,
            requested_effect: EffectClass::Workspace,
        };
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let allowed = gate.check(request, &context).await.unwrap();
        let task = TaskId(format!("t-{name}"));
        let lineage = crate::hub::lineage::Lineage::root_child(
            crate::hub::lineage::RootId::legacy(task.clone()),
        );
        SessionBinding::from_proof(&allowed, task, lineage)
    }

    pub(crate) async fn implementer() -> SessionBinding {
        binding("rust-implementer", DevLoop::Inner).await
    }

    pub(crate) async fn shepherd() -> SessionBinding {
        binding("pr-shepherd", DevLoop::Middle).await
    }

    pub(crate) fn scope(name: &str, max_effect: EffectClass, tools: &[&str]) -> RecipientScope {
        RecipientScope {
            session: SessionId::from_task(TaskId(format!("t-{name}"))),
            identity: name.to_string(),
            max_effect,
            tools: tools.iter().map(|t| t.to_string()).collect(),
            write_paths: vec!["api/**".to_string()],
            read_paths: None,
        }
    }

    pub(crate) fn with_paths(
        mut scope: RecipientScope,
        write: &[&str],
        read: Option<&[&str]>,
    ) -> RecipientScope {
        scope.write_paths = write.iter().map(|p| p.to_string()).collect();
        scope.read_paths = read.map(|r| r.iter().map(|p| p.to_string()).collect());
        scope
    }

    pub(crate) fn thread_msg(from: &str, subject: &str, body: &str) -> ThreadMessage {
        ThreadMessage {
            from: SessionId::from_task(TaskId(format!("t-{from}"))),
            from_identity: from.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
        }
    }

    #[derive(Clone)]
    pub(crate) enum Script {
        Reply(String),
        NoContent,
        Fail(String),
        Hang,
    }

    pub(crate) struct ScriptedProvider {
        script: Script,
        pub(crate) requests: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedProvider {
        pub(crate) fn new(script: Script) -> Arc<Self> {
            Arc::new(Self {
                script,
                requests: Mutex::new(Vec::new()),
            })
        }

        pub(crate) fn replying(reply: &str) -> Arc<Self> {
            Self::new(Script::Reply(reply.to_string()))
        }

        pub(crate) fn calls(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        pub(crate) fn user_prompt(&self) -> String {
            let requests = self.requests.lock().unwrap();
            requests[0].messages[1].content.clone().unwrap()
        }
    }

    fn response(content: Option<String>) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
            self.requests.lock().unwrap().push(request);
            match &self.script {
                Script::Reply(text) => Ok(response(Some(text.clone()))),
                Script::NoContent => Ok(response(None)),
                Script::Fail(message) => Err(ModelError::ServiceUnavailable {
                    message: message.clone(),
                }),
                Script::Hang => std::future::pending().await,
            }
        }

        async fn list_models(&self) -> ModelResult<Vec<model::types::ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "scripted"
        }
    }

    #[tokio::test]
    async fn the_scripted_provider_is_healthy_lists_nothing_and_has_a_name() {
        let p = ScriptedProvider::replying("x");
        p.health_check().await.unwrap();
        assert!(p.list_models().await.unwrap().is_empty());
        assert_eq!(p.provider_name(), "scripted");
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::auditor::ReasonCode;
    use crate::identity::ToolPattern;
    use proptest::prelude::*;

    #[test]
    fn hashing_is_deterministic_and_sensitive_to_every_byte() {
        assert_eq!(
            message_content_hash("s", "b"),
            message_content_hash("s", "b")
        );
        assert_ne!(
            message_content_hash("s", "b"),
            message_content_hash("s", "c")
        );
        assert_ne!(
            message_content_hash("s", "b"),
            message_content_hash("t", "b")
        );
        assert_ne!(
            message_content_hash("s", "b"),
            message_content_hash("s", "b\n")
        );
    }

    #[test]
    fn moving_text_across_the_subject_body_boundary_changes_the_hash() {
        assert_ne!(
            message_content_hash("ab", "c"),
            message_content_hash("a", "bc")
        );
        assert_ne!(
            message_content_hash("", "abc"),
            message_content_hash("abc", "")
        );
        assert_ne!(
            message_content_hash("1:", "2:x"),
            message_content_hash("1:2", ":x")
        );
    }

    proptest! {
        #[test]
        fn shifting_any_char_across_the_boundary_changes_the_hash(
            subject in ".{0,20}",
            head in proptest::char::any(),
            tail in ".{0,20}",
        ) {
            let before = message_content_hash(&subject, &format!("{head}{tail}"));
            let after = message_content_hash(&format!("{subject}{head}"), &tail);
            prop_assert_ne!(before, after);
        }
    }

    #[test]
    fn a_bound_verdict_covers_exactly_the_audited_content() {
        let bound = BoundVerdict::new(MessageVerdict::Deliver, "status", "all green");
        assert!(bound.binds("status", "all green"));
        assert!(!bound.binds("status", "all green!"));
        assert!(!bound.binds("statu", "sall green"));
        assert!(!bound.binds("", ""));
    }

    #[test]
    fn debug_output_never_contains_message_text() {
        let planted = PLANTED;
        let m = thread_msg("a", planted, planted);
        let shown = format!("{m:?}");
        assert!(!shown.contains("harpoon"));
        assert!(shown.contains(&format!("body_len: {}", planted.len())));
        assert!(shown.contains(&message_content_hash(planted, planted)));
    }

    #[tokio::test]
    async fn review_debug_output_never_contains_message_text() {
        let sender = implementer().await;
        let recipient = shepherd().await;
        let m = thread_msg("a", PLANTED, PLANTED);
        let window = [thread_msg("a", PLANTED, PLANTED)];
        let recipients = [scope("b", EffectClass::Ci, &["read_file"])];
        let out = OutboundReview {
            sender: &sender,
            tool: "msg_send",
            recipients: &recipients,
            subject: PLANTED,
            body: PLANTED,
            window: &window,
            thread: &window,
        };
        let bound = BoundVerdict::new(MessageVerdict::Deliver, PLANTED, PLANTED);
        let inb = InboundReview {
            recipient: &recipient,
            sender: scope("a", EffectClass::Repository, &[]),
            message: &m,
            outbound: Some(&bound),
            thread: &window,
        };
        for shown in [format!("{out:?}"), format!("{inb:?}")] {
            assert!(!shown.contains("harpoon"), "{shown}");
            assert!(shown.contains("body_len"));
        }
    }

    #[tokio::test]
    async fn a_scope_comes_from_the_binding_card() {
        let scope = RecipientScope::of(&shepherd().await);
        assert_eq!(scope.identity, "pr-shepherd");
        assert_eq!(scope.max_effect, EffectClass::Ci);
        assert!(scope.tools.contains(&"git_*".to_string()));
        assert_eq!(scope.write_paths, ["api/**", "shared/**", "tests/**"]);
        assert_eq!(scope.read_paths, Some(vec!["**".to_string()]));
        assert_eq!(RecipientScope::of(&implementer().await).read_paths, None);
        assert_eq!(scope.session.to_string(), "t-pr-shepherd");
    }

    #[test]
    fn a_higher_effect_or_an_extra_tool_is_asymmetric() {
        let low = scope("l", EffectClass::Workspace, &["read_file"]);
        let high = scope("h", EffectClass::Ci, &["read_file"]);
        assert!(is_privilege_asymmetric(&high, &low));
        assert!(!is_privilege_asymmetric(&low, &high));
        let more = scope(
            "m",
            EffectClass::Workspace,
            &["read_file", "git_push_branch"],
        );
        assert!(is_privilege_asymmetric(&more, &low));
        assert!(!is_privilege_asymmetric(&low, &more));
        assert!(!is_privilege_asymmetric(&low, &low));
    }

    #[test]
    fn a_wildcard_sender_does_not_cover_a_named_tool_so_the_check_over_triggers() {
        let sender = scope("s", EffectClass::Ci, &["git_*"]);
        let recipient = scope("r", EffectClass::Ci, &["git_push_branch"]);
        assert!(is_privilege_asymmetric(&recipient, &sender));
    }

    fn pathed(write: &[&str], read: Option<&[&str]>) -> RecipientScope {
        with_paths(
            scope("p", EffectClass::Repository, &["write_file"]),
            write,
            read,
        )
    }

    #[test]
    fn wider_recipient_write_paths_are_asymmetric_even_with_equal_effect_and_tools() {
        let sender = pathed(&["ui/**"], None);
        let recipient = pathed(&["api/**", ".github/**"], None);
        assert!(is_privilege_asymmetric(&recipient, &sender));
        let one_extra = pathed(&["ui/**", "docs/**"], None);
        assert!(is_privilege_asymmetric(&one_extra, &sender));
        let nothing_shared = pathed(&["api/**"], None);
        assert!(is_privilege_asymmetric(&nothing_shared, &sender));
    }

    #[test]
    fn equal_or_narrower_recipient_write_paths_are_symmetric() {
        let sender = pathed(&["ui/**", "docs/**"], None);
        assert!(!is_privilege_asymmetric(
            &pathed(&["docs/**", "ui/**"], None),
            &sender
        ));
        assert!(!is_privilege_asymmetric(&pathed(&["ui/**"], None), &sender));
        assert!(!is_privilege_asymmetric(&pathed(&[], None), &sender));
        assert!(!is_privilege_asymmetric(
            &pathed(&[], None),
            &pathed(&[], None)
        ));
    }

    #[test]
    fn a_recipient_with_extra_read_paths_is_asymmetric() {
        let sender = pathed(&["api/**"], Some(&["api/**"]));
        assert!(is_privilege_asymmetric(
            &pathed(&["api/**"], Some(&["api/**", "secrets/**"])),
            &sender
        ));
        assert!(is_privilege_asymmetric(&pathed(&["api/**"], None), &sender));
        assert!(!is_privilege_asymmetric(
            &pathed(&["api/**"], Some(&["api/**"])),
            &sender
        ));
        assert!(!is_privilege_asymmetric(
            &pathed(&["api/**"], Some(&[])),
            &sender
        ));
    }

    #[test]
    fn an_unrestricted_sender_reader_covers_any_recipient_reads() {
        let sender = pathed(&["api/**"], None);
        assert!(!is_privilege_asymmetric(
            &pathed(&["api/**"], Some(&["x/**"])),
            &sender
        ));
        assert!(!is_privilege_asymmetric(
            &pathed(&["api/**"], None),
            &sender
        ));
    }

    const PATH_POOL: [&str; 4] = ["api/**", "ui/**", ".github/**", "docs/**"];

    proptest! {
        #[test]
        fn path_widening_is_never_missed(
            rw in proptest::collection::btree_set(0usize..4, 0..4),
            sw in proptest::collection::btree_set(0usize..4, 0..4),
            rr in proptest::option::of(proptest::collection::btree_set(0usize..4, 0..4)),
            sr in proptest::option::of(proptest::collection::btree_set(0usize..4, 0..4)),
        ) {
            let names = |set: &std::collections::BTreeSet<usize>| -> Vec<&'static str> {
                set.iter().map(|i| PATH_POOL[*i]).collect()
            };
            let recipient = pathed(&names(&rw), rr.as_ref().map(&names).as_deref());
            let sender = pathed(&names(&sw), sr.as_ref().map(&names).as_deref());
            let writes_wider = rw.iter().any(|i| !sw.contains(i));
            let reads_wider = match (&rr, &sr) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(r), Some(s)) => r.iter().any(|i| !s.contains(i)),
            };
            prop_assert_eq!(
                is_privilege_asymmetric(&recipient, &sender),
                writes_wider || reads_wider
            );
        }
    }

    const UNIVERSE: [&str; 6] = [
        "read_file",
        "write_file",
        "git_push",
        "git_commit",
        "cargo_test",
        "msg_send",
    ];
    const PATTERNS: [&str; 8] = [
        "read_file",
        "write_file",
        "git_push",
        "git_*",
        "cargo_*",
        "*",
        "msg_?end",
        "git_commit",
    ];

    fn callable(patterns: &[String]) -> Vec<&'static str> {
        let compiled: Vec<ToolPattern> = patterns
            .iter()
            .map(|p| ToolPattern::new(p).unwrap())
            .collect();
        UNIVERSE
            .iter()
            .copied()
            .filter(|name| compiled.iter().any(|p| p.matches(name)))
            .collect()
    }

    fn scope_with(effect: EffectClass, picks: &[usize]) -> RecipientScope {
        let tools: Vec<&str> = picks.iter().map(|i| PATTERNS[*i]).collect();
        scope("p", effect, &tools)
    }

    fn any_effect() -> impl Strategy<Value = EffectClass> {
        proptest::sample::select(EffectClass::ALL.to_vec())
    }

    proptest! {
        #[test]
        fn the_gate_never_under_triggers(
            re in any_effect(), se in any_effect(),
            rt in proptest::collection::vec(0usize..PATTERNS.len(), 0..5),
            st in proptest::collection::vec(0usize..PATTERNS.len(), 0..5),
        ) {
            let recipient = scope_with(re, &rt);
            let sender = scope_with(se, &st);
            let can_recipient = callable(&recipient.tools);
            let can_sender = callable(&sender.tools);
            let truly_more = re > se || can_recipient.iter().any(|t| !can_sender.contains(t));
            if truly_more {
                prop_assert!(is_privilege_asymmetric(&recipient, &sender));
            }
        }

        #[test]
        fn with_exact_tool_names_the_gate_is_exact(
            re in any_effect(), se in any_effect(),
            rt in proptest::collection::vec(proptest::sample::select(vec![0usize, 1, 2, 7]), 0..4),
            st in proptest::collection::vec(proptest::sample::select(vec![0usize, 1, 2, 7]), 0..4),
        ) {
            let recipient = scope_with(re, &rt);
            let sender = scope_with(se, &st);
            let can_recipient = callable(&recipient.tools);
            let can_sender = callable(&sender.tools);
            let truly_more = re > se || can_recipient.iter().any(|t| !can_sender.contains(t));
            prop_assert_eq!(is_privilege_asymmetric(&recipient, &sender), truly_more);
        }
    }

    #[tokio::test]
    async fn the_rules_only_auditor_implements_the_trait() {
        let rules = RuleMessageAuditor::new();
        assert_eq!(MessageAuditor::name(&rules), RULE_MESSAGE_AUDITOR_NAME);
        let sender = implementer().await;
        let recipient = shepherd().await;
        let tok = token();
        let clean = OutboundReview {
            sender: &sender,
            tool: "msg_send",
            recipients: &[],
            subject: "status",
            body: "build is green",
            window: &[],
            thread: &[],
        };
        assert!(rules.review_outbound(&clean).await.unwrap().is_deliver());
        let leaking = OutboundReview {
            body: &format!("use {tok}"),
            ..clean
        };
        let verdict = rules.review_outbound(&leaking).await.unwrap();
        assert_eq!(verdict.reasons()[0].code, ReasonCode::SecretLeak);
        let half = &tok[..20];
        let rest = &tok[20..];
        let earlier = [thread_msg("a", "s", &format!("part {half}"))];
        let split = OutboundReview {
            body: rest,
            window: &earlier,
            ..clean
        };
        assert!(rules.review_outbound(&split).await.unwrap().is_block());
        let m = thread_msg("a", "s", &format!("use {tok}"));
        let inbound = InboundReview {
            recipient: &recipient,
            sender: RecipientScope::of(&sender),
            message: &m,
            outbound: None,
            thread: &[],
        };
        assert!(rules.review_inbound(&inbound).await.unwrap().is_block());
        let ok = thread_msg("a", "s", "fine");
        let inbound = InboundReview {
            message: &ok,
            ..inbound
        };
        assert!(rules.review_inbound(&inbound).await.unwrap().is_deliver());
    }
}
