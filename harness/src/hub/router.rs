//! The action gate's view of the hub: who is calling, what the gate reviews, and what halts.

use super::error::HubError;
use super::lineage::{SessionBinding, SessionId};
use super::message::SendRequest;
use super::oracle::{ComposedOracle, GitObjectOracle, GitRunner, HubIdOracle};
use super::send::{AuditorProvider, MessageHub};
use super::store::{MessageStore, SessionStore};
use crate::action_auditor::{
    known_arguments, ActionAuditError, ActionContext, ActionDenied, ActionReview, ActionVerdict,
    FailClosedMessageAuditor, MessageAuditor, MessageVerdict, ModelMessageAuditor, NullOracle,
    OutboundRouter, SharedOracle,
};
use crate::auditor::{Reason, ReasonCode};
use async_trait::async_trait;
use model::ModelProvider;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

struct Entry {
    binding: Arc<SessionBinding>,
    workspace: Option<PathBuf>,
}

/// The host-owned table from a session to the binding and the workspace the host gave it.
///
/// Only the task runner writes it, from an audited identity; nothing an agent sends selects an
/// entry. An entry lives exactly as long as its [`BindingGuard`].
///
/// ```
/// use harness::hub::router::BindingTable;
///
/// assert!(BindingTable::new().is_empty());
/// ```
#[derive(Default)]
pub struct BindingTable {
    entries: Mutex<HashMap<SessionId, Entry>>,
}

impl BindingTable {
    /// An empty table.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enter `binding`; the entry goes when the returned guard drops, whatever the exit path.
    pub fn enter(self: &Arc<Self>, binding: SessionBinding) -> BindingGuard {
        let session = binding.session().clone();
        let binding = Arc::new(binding);
        let entry = Entry {
            binding: Arc::clone(&binding),
            workspace: None,
        };
        self.lock().insert(session.clone(), entry);
        BindingGuard {
            table: Arc::clone(self),
            session,
            binding,
        }
    }

    /// The binding the host entered for `session`.
    pub fn binding(&self, session: &SessionId) -> Option<Arc<SessionBinding>> {
        let entries = self.lock();
        let found = entries.get(session);
        found.map(|entry| Arc::clone(&entry.binding))
    }

    /// The workspace the host gave `session`, if it has recorded one.
    pub fn workspace(&self, session: &SessionId) -> Option<PathBuf> {
        let entries = self.lock();
        let found = entries.get(session);
        found.and_then(|entry| entry.workspace.clone())
    }

    #[cfg(test)]
    pub(crate) fn workspace_of_only_entry(&self) -> Option<PathBuf> {
        let entries = self.lock();
        let mut all = entries.values();
        match (all.next(), all.next()) {
            (Some(entry), None) => entry.workspace.clone(),
            _ => None,
        }
    }

    /// How many sessions are bound.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no session is bound.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// Removes its session's entry from the [`BindingTable`] when dropped.
pub struct BindingGuard {
    table: Arc<BindingTable>,
    session: SessionId,
    binding: Arc<SessionBinding>,
}

impl BindingGuard {
    /// Record the workspace the host created for this session.
    pub fn set_workspace(&self, path: PathBuf) {
        let mut entries = self.table.lock();
        let ours = entries
            .get_mut(&self.session)
            .filter(|entry| Arc::ptr_eq(&entry.binding, &self.binding));
        if let Some(entry) = ours {
            entry.workspace = Some(path);
        }
    }
}

fn holds(
    entries: &HashMap<SessionId, Entry>,
    session: &SessionId,
    binding: &Arc<SessionBinding>,
) -> bool {
    let found = entries.get(session);
    found.is_some_and(|entry| Arc::ptr_eq(&entry.binding, binding))
}

impl Drop for BindingGuard {
    fn drop(&mut self) {
        let mut entries = self.table.lock();
        if holds(&entries, &self.session, &self.binding) {
            entries.remove(&self.session);
        }
    }
}

/// Builds the message auditor for one sender, with oracles scoped to that sender: the git-object
/// oracle runs in the sender's own workspace and only when the host recorded one; without it the
/// git side vouches for nothing.
pub struct ProductionAuditors {
    provider: Arc<dyn ModelProvider>,
    model: String,
    sessions: Arc<dyn SessionStore>,
    messages: Arc<dyn MessageStore>,
    table: Arc<BindingTable>,
    git: Arc<dyn GitRunner>,
}

impl ProductionAuditors {
    /// Auditors that review with `model` through `provider`.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: &str,
        sessions: Arc<dyn SessionStore>,
        messages: Arc<dyn MessageStore>,
        table: Arc<BindingTable>,
        git: Arc<dyn GitRunner>,
    ) -> Self {
        Self {
            provider,
            model: model.to_string(),
            sessions,
            messages,
            table,
            git,
        }
    }
}

impl ProductionAuditors {
    fn oracle_for(&self, sender: &SessionBinding) -> SharedOracle {
        let git: SharedOracle = match self.table.workspace(sender.session()) {
            Some(dir) => Arc::new(GitObjectOracle::new(dir, Arc::clone(&self.git))),
            None => Arc::new(NullOracle),
        };
        let ids = HubIdOracle::new(
            sender.lineage().root().clone(),
            Arc::clone(&self.sessions),
            Arc::clone(&self.messages),
        );
        Arc::new(ComposedOracle::new(git, Arc::new(ids)))
    }
}

impl AuditorProvider for ProductionAuditors {
    fn for_sender(&self, sender: &SessionBinding) -> Arc<dyn MessageAuditor> {
        let oracle = self.oracle_for(sender);
        let model = ModelMessageAuditor::new(Arc::clone(&self.provider), self.model.clone());
        let model = model.with_oracle(Arc::clone(&oracle));
        Arc::new(FailClosedMessageAuditor::new(model).with_oracle(oracle))
    }
}

/// The reason line the agent sees for a code: fixed text, never a matched string.
fn fixed(code: ReasonCode) -> Reason {
    Reason::new(code, format!("message withheld: {}", code.as_str()))
}

fn sanitized(verdict: &MessageVerdict) -> Vec<Reason> {
    let reasons = verdict.reasons();
    reasons.iter().map(|reason| fixed(reason.code)).collect()
}

fn refuse(detail: &str) -> ActionVerdict {
    ActionVerdict::block(vec![Reason::new(ReasonCode::Other, detail)])
}

/// What a quarantine inside a tool must become so the sender halts: an escalation, which the
/// registry delivers to the human plane. `None` for every other error.
///
/// ```
/// use harness::hub::error::HubError;
/// use harness::hub::router::halting_denial;
///
/// assert!(halting_denial(&HubError::Quarantined { recipients: 1 }).unwrap().is_escalation());
/// assert!(halting_denial(&HubError::NotVisible).is_none());
/// ```
pub fn halting_denial(error: &HubError) -> Option<ActionDenied> {
    match error {
        HubError::Quarantined { .. } => Some(ActionDenied::Escalate {
            reasons: vec![Reason::new(
                ReasonCode::Other,
                "message held for human review; stop and wait",
            )],
        }),
        _ => None,
    }
}

/// Reviews the hub's message tools for the action gate.
///
/// The sender is the session the host bound for the task the call belongs to; a call with no bound
/// session is blocked. Message text never reaches a reason, a log line or an error.
pub struct HubRouter {
    hub: Arc<MessageHub>,
    table: Arc<BindingTable>,
}

impl HubRouter {
    /// A router over `hub` and the `table` the task runner fills.
    pub fn new(hub: Arc<MessageHub>, table: Arc<BindingTable>) -> Self {
        Self { hub, table }
    }

    /// The hub behind this router.
    pub fn hub(&self) -> Arc<MessageHub> {
        Arc::clone(&self.hub)
    }

    /// The table the task runner fills.
    pub fn table(&self) -> Arc<BindingTable> {
        Arc::clone(&self.table)
    }

    fn sender(&self, review: &ActionReview) -> Option<Arc<SessionBinding>> {
        let id = SessionId::from_task(review.task_id.clone());
        self.table.binding(&id)
    }
}

fn declared_only(review: &ActionReview) -> bool {
    let known = known_arguments(&review.tool);
    match review.args.as_object() {
        Some(map) => map.keys().all(|key| known.contains(&key.as_str())),
        None => false,
    }
}

fn note_of(review: &ActionReview) -> Option<&str> {
    review.args.get("note").and_then(serde_json::Value::as_str)
}

fn request_of(review: &ActionReview) -> Option<SendRequest> {
    serde_json::from_value(review.args.clone()).ok()
}

fn hub_refusal(error: &HubError) -> ActionVerdict {
    match error {
        HubError::AuditUnavailable | HubError::Store(_) => refuse("message review unavailable"),
        other => refuse(&other.to_string()),
    }
}

fn mapped(verdict: &MessageVerdict) -> ActionVerdict {
    match verdict {
        MessageVerdict::Deliver => ActionVerdict::Allow,
        MessageVerdict::Block { .. } => ActionVerdict::block(sanitized(verdict)),
        MessageVerdict::Quarantine { .. } => ActionVerdict::escalate(sanitized(verdict)),
    }
}

#[async_trait]
impl OutboundRouter for HubRouter {
    fn handles(&self, tool: &str) -> bool {
        tool == "msg_send" || tool == "session_set_note"
    }

    async fn review(
        &self,
        review: &ActionReview,
        _context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError> {
        let Some(sender) = self.sender(review) else {
            return Ok(refuse("no session is bound to this call"));
        };
        if !declared_only(review) {
            return Ok(refuse(
                "the arguments are not the tool's declared arguments",
            ));
        }
        let outcome = if review.tool == "msg_send" {
            match request_of(review) {
                Some(req) => self.hub.review_send(&sender, &req).await,
                None => return Ok(refuse("the message arguments are not valid")),
            }
        } else {
            match note_of(review) {
                Some(note) => self.hub.review_note(&sender, note).await,
                None => return Ok(refuse("the note argument is not valid")),
            }
        };
        Ok(match outcome {
            Ok(verdict) => mapped(&verdict),
            Err(error) => hub_refusal(&error),
        })
    }

    async fn on_quarantine(
        &self,
        review: &ActionReview,
        reasons: &[Reason],
    ) -> Result<(), ActionAuditError> {
        let unrecorded = || ActionAuditError::Log(std::io::Error::other("no quarantine record"));
        let sender = self.sender(review).ok_or_else(unrecorded)?;
        let codes: Vec<ReasonCode> = reasons.iter().map(|reason| reason.code).collect();
        if review.tool == "msg_send" {
            let req = request_of(review).ok_or_else(unrecorded)?;
            let stored = self.hub.quarantine_send(&sender, &req, codes);
            stored.map(drop).map_err(|_| unrecorded())
        } else {
            let note = note_of(review).ok_or_else(unrecorded)?;
            self.hub.quarantine_note(&sender, note, codes);
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::action_auditor::{
        ActionAuditLog, ActionAuditor, ActionGate, InboundReview, OutboundReview,
        RoutingActionAuditor,
    };
    use crate::effects::EffectClass;
    use crate::hub::lineage::{Lineage, RootId};
    use crate::hub::message::{MessageKind, MessageReach};
    use crate::hub::registry::SessionRegistry;
    use crate::hub::send::{QuarantineEvent, QuarantineSink, GATE_AUDITOR};
    use crate::hub::store::{InMemoryMessageStore, InMemorySessionStore};
    use crate::leases::SimulatedClock;
    use crate::task::TaskId;
    use crate::tools::{ActionSubject, Tool, ToolError, ToolRegistry, ToolResult};
    use chrono::{TimeZone, Utc};
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    pub(crate) fn token() -> String {
        format!("{}_{}", "ghp", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8")
    }

    fn at() -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    pub(crate) struct Scripted {
        replies: Mutex<VecDeque<Result<MessageVerdict, ()>>>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(replies: Vec<Result<MessageVerdict, ()>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                calls: AtomicUsize::new(0),
            })
        }

        pub(crate) fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MessageAuditor for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn review_outbound(
            &self,
            _r: &OutboundReview<'_>,
        ) -> Result<MessageVerdict, ActionAuditError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let next = self.replies.lock().unwrap().pop_front();
            match next.unwrap_or(Ok(MessageVerdict::Deliver)) {
                Ok(verdict) => Ok(verdict),
                Err(()) => Err(ActionAuditError::Timeout {
                    timeout: std::time::Duration::from_secs(1),
                }),
            }
        }

        async fn review_inbound(
            &self,
            _r: &InboundReview<'_>,
        ) -> Result<MessageVerdict, ActionAuditError> {
            Ok(MessageVerdict::Deliver)
        }
    }

    #[derive(Default)]
    pub(crate) struct Recording(
        pub(crate) Mutex<Vec<QuarantineEvent>>,
        pub(crate) std::sync::atomic::AtomicBool,
    );

    impl QuarantineSink for Recording {
        fn quarantined(&self, event: &QuarantineEvent) -> Result<(), String> {
            self.0.lock().unwrap().push(event.clone());
            match self.1.load(Ordering::SeqCst) {
                true => Err("sink down".to_string()),
                false => Ok(()),
            }
        }
    }

    pub(crate) struct Counting(AtomicUsize);

    #[async_trait]
    impl ActionAuditor for Counting {
        fn name(&self) -> &str {
            "counting"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ActionVerdict::Allow)
        }
    }

    fn definition_of(name: &str) -> model::types::ToolDefinition {
        model::types::ToolDefinition {
            function: model::types::FunctionDefinition {
                name: name.to_string(),
                description: String::new(),
                parameters: model::types::JsonSchema {
                    schema_type: model::types::SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        }
    }

    struct SendTool {
        hub: Arc<MessageHub>,
        sender: Arc<SessionBinding>,
    }

    #[async_trait]
    impl Tool for SendTool {
        fn definition(&self) -> model::types::ToolDefinition {
            definition_of(self.name())
        }

        async fn execute(&self, args: Value) -> ToolResult<Value> {
            let req: SendRequest = serde_json::from_value(args)?;
            match self.hub.send(&self.sender, req).await {
                Ok(receipt) => Ok(json!({"recipients": receipt.recipients})),
                Err(error) => Err(match halting_denial(&error) {
                    Some(denied) => ToolError::ActionDenied(denied),
                    None => ToolError::ExecutionFailed {
                        message: error.to_string(),
                    },
                }),
            }
        }

        fn name(&self) -> &str {
            "msg_send"
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::None
        }
    }

    struct NoteTool;

    #[async_trait]
    impl Tool for NoteTool {
        fn definition(&self) -> model::types::ToolDefinition {
            definition_of(self.name())
        }

        async fn execute(&self, _args: Value) -> ToolResult<Value> {
            Ok(json!({}))
        }

        fn name(&self) -> &str {
            "session_set_note"
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::None
        }
    }

    pub(crate) struct Rig {
        pub(crate) messages: Arc<InMemoryMessageStore>,
        pub(crate) hub: Arc<MessageHub>,
        pub(crate) table: Arc<BindingTable>,
        pub(crate) router: Arc<HubRouter>,
        pub(crate) auditor: Arc<Scripted>,
        pub(crate) sink: Arc<Recording>,
        pub(crate) pager: Arc<crate::hub::quarantine_sink::EscalationQuarantineSink>,
        pub(crate) sender: Arc<SessionBinding>,
        pub(crate) peer: SessionBinding,
        pub(crate) default: Arc<Counting>,
        pub(crate) escalations: Arc<crate::test_support::escalation::CapturingSink>,
        pub(crate) registry: ToolRegistry,
        pub(crate) log_dir: tempfile::TempDir,
        pub(crate) gate: Arc<ActionGate>,
        pub(crate) subject: ActionSubject,
        pub(crate) escalator: Arc<crate::escalation::Escalator>,
        pub(crate) _guard: Option<BindingGuard>,
    }

    pub(crate) fn binding(registry: &SessionRegistry, name: &str, root: &RootId) -> SessionBinding {
        let mut card = crate::identity::example();
        card.scope.message_reach = MessageReach::Root;
        let lineage = Lineage::root_child(root.clone());
        let binding = SessionBinding::from_audited(&card, TaskId(name.to_string()), lineage);
        registry.register(&binding, "subtask", at()).unwrap();
        binding
    }

    pub(crate) fn rig(replies: Vec<Result<MessageVerdict, ()>>, bound: bool) -> Rig {
        rig_with(replies, bound, Default::default())
    }

    pub(crate) fn rig_with(
        replies: Vec<Result<MessageVerdict, ()>>,
        bound: bool,
        limits: crate::hub::limits::HubLimits,
    ) -> Rig {
        rig_built(replies, bound, limits, false)
    }

    pub(crate) fn rig_paging(replies: Vec<Result<MessageVerdict, ()>>, bound: bool) -> Rig {
        rig_built(replies, bound, Default::default(), true)
    }

    struct Both(
        Arc<Recording>,
        Arc<crate::hub::quarantine_sink::EscalationQuarantineSink>,
    );

    impl QuarantineSink for Both {
        fn quarantined(&self, event: &QuarantineEvent) -> Result<(), String> {
            self.0.quarantined(event)?;
            self.1.quarantined(event)
        }
    }

    fn rig_built(
        replies: Vec<Result<MessageVerdict, ()>>,
        bound: bool,
        limits: crate::hub::limits::HubLimits,
        paging: bool,
    ) -> Rig {
        let escalations = crate::test_support::escalation::CapturingSink::new(false);
        let (escalator, _) = crate::test_support::escalation::escalator(&escalations);
        let pager = crate::hub::quarantine_sink::EscalationQuarantineSink::new(
            crate::test_support::escalation::plane(&escalations),
        );
        let pager = Arc::new(pager);
        let sessions = Arc::new(InMemorySessionStore::new());
        let messages = Arc::new(InMemoryMessageStore::new());
        let session_registry = Arc::new(SessionRegistry::new(sessions.clone()));
        let root = RootId::mcp(Uuid::from_u128(7));
        let sender = Arc::new(binding(&session_registry, "sender", &root));
        let peer = binding(&session_registry, "peer", &root);
        let auditor = Scripted::new(replies);
        let shared: Arc<dyn MessageAuditor> = auditor.clone();
        let sink = Arc::new(Recording::default());
        let hub_sink: Arc<dyn QuarantineSink> = match paging {
            true => Arc::new(Both(sink.clone(), pager.clone())),
            false => sink.clone(),
        };
        let hub = MessageHub::new(
            sessions,
            messages.clone(),
            Arc::new(shared),
            Arc::new(SimulatedClock::new(at())),
            limits,
            session_registry,
        )
        .with_quarantine_sink(hub_sink);
        let hub = Arc::new(hub);
        let table = BindingTable::new();
        let router = Arc::new(HubRouter::new(hub.clone(), table.clone()));
        let guard = bound.then(|| {
            table.enter(SessionBinding::from_audited(
                sender.identity(),
                TaskId("sender".to_string()),
                sender.lineage().clone(),
            ))
        });
        let default = Arc::new(Counting(AtomicUsize::new(0)));
        let routing = RoutingActionAuditor::new(Arc::downgrade(&router), default.clone());
        let log_dir = tempfile::tempdir().unwrap();
        let log = ActionAuditLog::file(log_dir.path().join("action_audit.jsonl"));
        let gate = Arc::new(ActionGate::new(Arc::new(routing), log));
        let subject = ActionSubject {
            task_id: TaskId("sender".to_string()),
            max_effect: EffectClass::None,
            window: None,
            repo: "example/repo".to_string(),
            branch: None,
            pr: None,
            environment: None,
            paths: vec![],
            description: String::new(),
        };
        let mut registry = ToolRegistry::new()
            .with_action_gate(gate.clone(), subject.clone())
            .with_action_escalator(escalator.clone());
        registry.register(Box::new(SendTool {
            hub: hub.clone(),
            sender: sender.clone(),
        }));
        registry.register(Box::new(NoteTool));
        Rig {
            messages,
            hub,
            table,
            router,
            auditor,
            sink,
            pager,
            sender,
            peer,
            default,
            escalations,
            registry,
            log_dir,
            gate,
            subject,
            escalator,
            _guard: guard,
        }
    }

    async fn call(rig: &Rig, tool: &str, args: Value) -> ToolResult<Value> {
        rig.registry.execute(tool, args).await
    }

    pub(crate) fn args(rig: &Rig, body: &str) -> Value {
        json!({
            "to": {"to": "session", "id": rig.peer.session()},
            "kind": MessageKind::Info,
            "subject": "status",
            "body": body,
        })
    }

    pub(crate) fn unread(rig: &Rig) -> usize {
        rig.messages.unread(rig.peer.session().clone()).unwrap()
    }

    pub(crate) fn denied(result: ToolResult<Value>) -> crate::action_auditor::ActionDenied {
        use ToolError::ActionDenied as Den;
        let error = result.unwrap_err();
        let Den(denied) = error else { unreachable!() };
        denied
    }

    pub(crate) fn quarantine(code: ReasonCode, detail: &str) -> MessageVerdict {
        MessageVerdict::Quarantine {
            reasons: vec![Reason::new(code, detail)],
        }
    }

    pub(crate) fn block(code: ReasonCode, detail: &str) -> MessageVerdict {
        MessageVerdict::Block {
            reasons: vec![Reason::new(code, detail)],
        }
    }

    pub(crate) fn everything(rig: &Rig) -> String {
        let file = std::fs::read_to_string(rig.log_dir.path().join("action_audit.jsonl"))
            .unwrap_or_default();
        format!(
            "{file}{:?}{:?}{:?}",
            rig.registry.action_reviews(),
            rig.escalations.escalations(),
            rig.sink.0.lock().unwrap()
        )
    }

    #[tokio::test]
    async fn a_delivered_message_is_reviewed_by_the_gate_and_again_by_the_hub() {
        let rig = rig(vec![], true);
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        assert_eq!(out.unwrap(), json!({"recipients": 1}));
        assert_eq!(rig.auditor.calls(), 2);
        assert_eq!(unread(&rig), 1);
        assert_eq!(rig.default.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_call_with_no_bound_session_is_blocked_and_nothing_is_reviewed_or_stored() {
        let rig = rig(vec![], false);
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        let denied = denied(out);
        assert!(!denied.is_escalation());
        assert_eq!(rig.auditor.calls(), 0);
        assert_eq!(unread(&rig), 0);
        assert_eq!(rig.default.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_gate_block_wins_and_the_hub_is_never_reached() {
        let rig = rig(vec![Ok(block(ReasonCode::PromptInjection, "rule 3"))], true);
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        let denied = denied(out);
        assert!(!denied.is_escalation());
        assert_eq!(denied.reasons()[0].code, ReasonCode::PromptInjection);
        assert_eq!(rig.auditor.calls(), 1);
        assert_eq!(unread(&rig), 0);
        assert!(rig.sink.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_hub_block_after_a_gate_deliver_wins_and_stores_nothing() {
        let rig = rig(
            vec![
                Ok(MessageVerdict::Deliver),
                Ok(block(ReasonCode::SecretLeak, "x")),
            ],
            true,
        );
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        assert!(matches!(out, Err(ToolError::ExecutionFailed { .. })));
        assert_eq!(rig.auditor.calls(), 2);
        assert_eq!(unread(&rig), 0);
        assert!(rig.escalations.escalations().is_empty());
    }

    #[tokio::test]
    async fn a_gate_quarantine_halts_the_sender_and_the_hub_is_never_reached() {
        let rig = rig(vec![Ok(quarantine(ReasonCode::SecretLeak, "rule 9"))], true);
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        assert!(denied(out).is_escalation());
        assert_eq!(rig.auditor.calls(), 1);
        assert_eq!(unread(&rig), 0);
        assert_eq!(rig.escalations.escalations().len(), 1);
        let events = rig.sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].codes, vec![ReasonCode::SecretLeak]);
        assert_eq!(events[0].ids.len(), 1);
        let stored = rig.messages.get(events[0].ids[0]).unwrap().unwrap();
        assert_eq!(
            stored.state,
            crate::hub::message::DeliveryState::Quarantined
        );
        assert_eq!(stored.body, "[withheld: secret_leak; 5 bytes]");
        assert_eq!(stored.outbound_verdict.log_id, GATE_AUDITOR);
    }

    #[tokio::test]
    async fn a_hub_quarantine_after_a_gate_deliver_also_halts_the_sender() {
        let rig = rig(
            vec![
                Ok(MessageVerdict::Deliver),
                Ok(quarantine(ReasonCode::CrossScopeSteering, "rule 9")),
            ],
            true,
        );
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        assert!(denied(out).is_escalation());
        assert_eq!(rig.auditor.calls(), 2);
        assert_eq!(rig.escalations.escalations().len(), 1);
        assert_eq!(rig.sink.0.lock().unwrap().len(), 1);
        assert_eq!(unread(&rig), 0);
    }

    #[tokio::test]
    async fn a_note_goes_through_the_full_review_and_a_quarantine_stores_no_text() {
        let rig = rig(
            vec![
                Ok(MessageVerdict::Deliver),
                Ok(quarantine(ReasonCode::SecretLeak, "rule 9")),
            ],
            true,
        );
        let note = json!({"note": "reading the parser"});
        let first = call(&rig, "session_set_note", note.clone()).await;
        assert!(first.is_ok());
        assert_eq!(rig.auditor.calls(), 1);
        let out = rig.registry.execute("session_set_note", note).await;
        assert!(denied(out).is_escalation());
        assert_eq!(rig.escalations.escalations().len(), 1);
        let events = rig.sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].ids.is_empty());
        assert_eq!(events[0].body_len, "reading the parser".len());
        assert_eq!(unread(&rig), 0);
    }

    #[tokio::test]
    async fn a_failing_auditor_blocks_without_text_and_stores_nothing() {
        let rig = rig(vec![Err(())], true);
        let out = rig.registry.execute("msg_send", args(&rig, "hello")).await;
        let denied = denied(out);
        assert!(!denied.is_escalation());
        assert_eq!(denied.reasons()[0].detail, "message review unavailable");
        assert_eq!(unread(&rig), 0);
    }

    #[tokio::test]
    async fn a_planted_token_appears_in_no_audit_output_for_any_verdict() {
        let planted = token();
        for verdict in [
            block(ReasonCode::SecretLeak, &planted),
            quarantine(ReasonCode::SecretLeak, &planted),
            quarantine(ReasonCode::CrossScopeSteering, &planted),
        ] {
            let rig = rig(vec![Ok(verdict)], true);
            let out = rig.registry.execute("msg_send", args(&rig, &planted)).await;
            let text = format!("{:?}{}", out, everything(&rig));
            assert!(!text.contains(&planted), "{text}");
            let verdicts = format!("{:?}", rig.registry.action_reviews());
            assert!(!verdicts.contains("ghp_"), "{verdicts}");
        }
        let rig = rig(vec![], true);
        let mut forged = args(&rig, "hello");
        forged[planted.as_str()] = json!(1);
        let out = rig.registry.execute("msg_send", forged).await;
        let text = format!("{:?}{}", out, everything(&rig));
        assert!(!text.contains(&planted), "{text}");
        assert!(matches!(out, Err(ToolError::ActionDenied(_))));
        let both = json!({"note": planted.clone(), "x": planted.clone()});
        let note = call(&rig, "session_set_note", both).await;
        assert!(!format!("{note:?}{}", everything(&rig)).contains(&planted));
    }

    #[tokio::test]
    async fn malformed_arguments_are_blocked_with_fixed_text() {
        for bad in [json!({"to": "session"}), json!("body"), json!([1])] {
            let rig = rig(vec![], true);
            let out = rig.registry.execute("msg_send", bad).await;
            assert!(!denied(out).is_escalation());
        }
        let rig = rig(vec![], true);
        let out = call(&rig, "session_set_note", json!({"note": 5})).await;
        assert_eq!(
            denied(out).reasons()[0].detail,
            "the note argument is not valid"
        );
        let out = rig.registry.execute("msg_send", json!({"zzz": 1})).await;
        assert_eq!(
            denied(out).reasons()[0].detail,
            "the arguments are not the tool's declared arguments"
        );
        assert_eq!(rig.auditor.calls(), 0);
    }

    #[tokio::test]
    async fn an_unhandled_hub_error_blocks_with_its_own_fixed_text() {
        let rig = rig(vec![], true);
        let mut to_nobody = args(&rig, "hello");
        to_nobody["to"]["id"] = json!("not-a-session");
        let out = rig.registry.execute("msg_send", to_nobody).await;
        assert_eq!(denied(out).reasons()[0].detail, "session not visible");
        let long = "x".repeat(200_000);
        let out = rig.registry.execute("msg_send", args(&rig, &long)).await;
        assert!(denied(out).reasons()[0].detail.starts_with("body exceeds"));
        let note = "n".repeat(500);
        let out = call(&rig, "session_set_note", json!({"note": note})).await;
        assert!(denied(out).reasons()[0].detail.starts_with("note exceeds"));
    }

    #[tokio::test]
    async fn a_sink_that_is_down_does_not_stop_a_note_quarantine_from_halting() {
        let rig = rig(
            vec![Ok(MessageVerdict::Quarantine { reasons: vec![] })],
            true,
        );
        rig.sink.1.store(true, Ordering::SeqCst);
        let out = call(&rig, "session_set_note", json!({"note": "x"})).await;
        assert!(denied(out).is_escalation());
        assert_eq!(rig.sink.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_tools_declare_themselves_and_the_routing_auditor_names_its_default() {
        let rig = rig(vec![], true);
        let mut names: Vec<String> = rig
            .registry
            .get_definitions()
            .iter()
            .map(|definition| definition.function.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, ["msg_send", "session_set_note"]);
        let routing = RoutingActionAuditor::new(Arc::downgrade(&rig.router), rig.default.clone());
        assert_eq!(routing.name(), "counting");
    }

    #[test]
    fn the_production_auditors_build_one_auditor_per_sender() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let registry = SessionRegistry::new(sessions.clone());
        let sender = binding(&registry, "mine", &RootId::mcp(Uuid::from_u128(3)));
        let auditors = ProductionAuditors::new(
            Arc::new(model::OllamaProvider::new(model::OllamaConfig::default()).unwrap()),
            "m",
            sessions,
            Arc::new(InMemoryMessageStore::new()),
            BindingTable::new(),
            Arc::new(Runner(Mutex::new(Vec::new()))),
        );
        let first = auditors.for_sender(&sender);
        let second = auditors.for_sender(&sender);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!first.name().is_empty());
    }

    fn limited(sender: u32, root: u32, inbox: usize) -> crate::hub::limits::HubLimits {
        crate::hub::limits::HubLimits {
            per_sender_per_10m: sender,
            per_root_per_hour: root,
            inbox_max: inbox,
            ..Default::default()
        }
    }

    fn send_request(rig: &Rig) -> SendRequest {
        serde_json::from_value(args(rig, "hello")).unwrap()
    }

    fn gate_review(tool: &str, args: Value) -> ActionReview {
        ActionReview {
            identity: "x".into(),
            task_id: TaskId("sender".into()),
            tool: tool.into(),
            args,
            effect_class: EffectClass::None,
            prior_actions: vec![],
        }
    }

    async fn gate_verdict(rig: &Rig, tool: &str, args: Value) -> ActionVerdict {
        let (review, cx) = (gate_review(tool, args), gate_context());
        rig.router.review(&review, &cx).await.unwrap()
    }

    fn gate_context() -> ActionContext<'static> {
        ActionContext {
            max_effect: EffectClass::None,
            window: None,
            lease: crate::leases::LeaseContext::default(),
            now: Utc::now(),
            task: "",
        }
    }

    async fn assert_refused_without_review(rig: &Rig, tool: &str, args: Value, detail: &str) {
        let before = rig.auditor.calls();
        for _ in 0..3 {
            let verdict = gate_verdict(rig, tool, args.clone()).await;
            let ActionVerdict::Block { reasons } = verdict else {
                unreachable!()
            };
            assert!(
                reasons[0].detail.starts_with(detail),
                "{}",
                reasons[0].detail
            );
        }
        assert_eq!(rig.auditor.calls(), before);
    }

    #[tokio::test]
    async fn a_sender_over_its_window_gets_no_gate_review() {
        let rig = rig_with(vec![], true, limited(2, 100, 100));
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        let out = args(&rig, "again");
        assert_refused_without_review(&rig, "msg_send", out, "rate limited").await;
    }

    #[tokio::test]
    async fn a_root_over_its_hourly_cap_gets_no_gate_review() {
        let rig = rig_with(vec![], true, limited(100, 1, 100));
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        let out = args(&rig, "again");
        assert_refused_without_review(&rig, "msg_send", out, "rate limited").await;
    }

    #[tokio::test]
    async fn a_full_recipient_inbox_gets_no_gate_review() {
        let rig = rig_with(vec![], true, limited(100, 100, 1));
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        let out = args(&rig, "again");
        assert_refused_without_review(&rig, "msg_send", out, "recipient inbox full").await;
    }

    #[tokio::test]
    async fn a_note_over_the_window_or_the_root_cap_gets_no_gate_review() {
        let note = json!({"note": "reading"});
        let rig = rig_with(vec![], true, limited(1, 100, 100));
        rig.hub.set_note(&rig.sender, "first").unwrap();
        assert_refused_without_review(&rig, "session_set_note", note.clone(), "rate limited").await;
        let rig = rig_with(vec![], true, limited(100, 1, 100));
        rig.hub.set_note(&rig.sender, "first").unwrap();
        assert_refused_without_review(&rig, "session_set_note", note, "rate limited").await;
    }

    #[tokio::test]
    async fn the_gate_review_takes_no_slot_and_no_inbox_room() {
        let rig = rig_with(vec![], true, limited(2, 100, 3));
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        for _ in 0..5 {
            let verdict = gate_verdict(&rig, "msg_send", args(&rig, "hello")).await;
            assert!(matches!(verdict, ActionVerdict::Allow));
            let noted = gate_verdict(&rig, "session_set_note", json!({"note": "x"})).await;
            assert!(matches!(noted, ActionVerdict::Allow));
        }
        rig.hub.send(&rig.sender, send_request(&rig)).await.unwrap();
        let over = rig.hub.send(&rig.sender, send_request(&rig)).await;
        assert!(matches!(over, Err(HubError::RateLimited(_))), "{over:?}");
        assert_eq!(unread(&rig), 2);
    }

    #[test]
    fn the_router_handles_exactly_the_two_tools_it_reviews() {
        let rig = rig(vec![], true);
        assert!(rig.router.handles("msg_send"));
        assert!(rig.router.handles("session_set_note"));
        for other in ["sessions_query", "msg_recv", "write_file", ""] {
            assert!(!rig.router.handles(other), "{other}");
        }
        assert!(Arc::ptr_eq(&rig.router.hub(), &rig.hub));
        assert!(Arc::ptr_eq(&rig.router.table(), &rig.table));
    }

    #[tokio::test]
    async fn a_gate_quarantine_of_an_unbound_or_malformed_call_reports_that_no_record_was_written()
    {
        let rig = rig(vec![], false);
        let review = ActionReview {
            identity: "x".into(),
            task_id: TaskId("sender".into()),
            tool: "msg_send".into(),
            args: args(&rig, "hello"),
            effect_class: EffectClass::None,
            prior_actions: vec![],
        };
        let reasons = [Reason::new(ReasonCode::SecretLeak, "x")];
        assert!(rig.router.on_quarantine(&review, &reasons).await.is_err());
        let bound = rig.table.enter(SessionBinding::from_audited(
            rig.sender.identity(),
            TaskId("sender".to_string()),
            rig.sender.lineage().clone(),
        ));
        let mut broken = review.clone();
        broken.args = json!({"nope": 1});
        assert!(rig.router.on_quarantine(&broken, &reasons).await.is_err());
        let mut gone = review.clone();
        gone.args["to"]["id"] = json!("not-a-session");
        assert!(rig.router.on_quarantine(&gone, &reasons).await.is_err());
        let mut note = review.clone();
        note.tool = "session_set_note".into();
        note.args = json!({});
        assert!(rig.router.on_quarantine(&note, &reasons).await.is_err());
        drop(bound);
    }

    #[test]
    fn a_quarantine_inside_a_tool_halts_and_nothing_else_does() {
        let halted = halting_denial(&HubError::Quarantined { recipients: 2 }).unwrap();
        assert!(halted.is_escalation());
        assert_eq!(
            halted.reasons()[0].detail,
            "message held for human review; stop and wait"
        );
        for other in [
            HubError::NotVisible,
            HubError::Blocked(vec![ReasonCode::SecretLeak]),
            HubError::AuditUnavailable,
        ] {
            assert!(halting_denial(&other).is_none(), "{other:?}");
        }
    }

    fn session(name: &str) -> (SessionId, SessionBinding) {
        let registry = SessionRegistry::new(Arc::new(InMemorySessionStore::new()));
        let b = binding(&registry, name, &RootId::mcp(Uuid::from_u128(9)));
        (b.session().clone(), b)
    }

    #[test]
    fn an_entry_lives_exactly_as_long_as_its_guard() {
        let table = BindingTable::new();
        assert!(table.is_empty());
        let (id, b) = session("a");
        let guard = table.enter(b);
        assert_eq!(table.len(), 1);
        assert!(table.binding(&id).is_some());
        drop(guard);
        assert!(table.binding(&id).is_none());
        assert!(table.is_empty());
    }

    #[test]
    fn a_panic_while_bound_still_removes_the_entry() {
        let table = BindingTable::new();
        let (id, b) = session("a");
        let inner = table.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = inner.enter(b);
            panic!("the task died");
        }));
        assert!(outcome.is_err());
        assert!(table.binding(&id).is_none());
    }

    #[test]
    fn the_workspace_is_the_hosts_and_goes_with_the_entry() {
        let table = BindingTable::new();
        let (id, b) = session("a");
        assert!(table.workspace(&id).is_none());
        let guard = table.enter(b);
        assert!(table.workspace(&id).is_none());
        guard.set_workspace(PathBuf::from("/work/a"));
        assert_eq!(table.workspace(&id), Some(PathBuf::from("/work/a")));
        drop(guard);
        assert!(table.workspace(&id).is_none());
        let (other, b2) = session("b");
        let (old, new) = (table.enter(b2), table.enter(session("b").1));
        old.set_workspace(PathBuf::from("/stale"));
        assert!(table.workspace(&other).is_none());
        new.set_workspace(PathBuf::from("/fresh"));
        assert_eq!(table.workspace(&other), Some(PathBuf::from("/fresh")));
    }

    #[test]
    fn an_old_guard_does_not_remove_a_newer_entry_for_the_same_session() {
        let table = BindingTable::new();
        let (id, first) = session("a");
        let second = SessionBinding::from_audited(
            first.identity(),
            TaskId("a".to_string()),
            first.lineage().clone(),
        );
        let old = table.enter(first);
        let new = table.enter(second);
        drop(old);
        assert!(table.binding(&id).is_some());
        drop(new);
        assert!(table.binding(&id).is_none());
    }

    struct Runner(Mutex<Vec<PathBuf>>);

    impl GitRunner for Runner {
        fn object_exists(&self, dir: &std::path::Path, _hex: &str) -> bool {
            self.0.lock().unwrap().push(dir.to_path_buf());
            true
        }
    }

    #[test]
    fn the_git_oracle_runs_only_in_the_senders_own_recorded_workspace() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let messages = Arc::new(InMemoryMessageStore::new());
        let registry = SessionRegistry::new(sessions.clone());
        let root = RootId::mcp(Uuid::from_u128(3));
        let (mine, other) = (
            binding(&registry, "mine", &root),
            binding(&registry, "other", &root),
        );
        let table = BindingTable::new();
        let runner = Arc::new(Runner(Mutex::new(Vec::new())));
        let auditors = ProductionAuditors::new(
            Arc::new(model::OllamaProvider::new(model::OllamaConfig::default()).unwrap()),
            "m",
            sessions,
            messages,
            table.clone(),
            runner.clone(),
        );
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let guard = table.enter(SessionBinding::from_audited(
            mine.identity(),
            TaskId("mine".into()),
            mine.lineage().clone(),
        ));
        assert!(!auditors.oracle_for(&mine).is_known_git_object(sha));
        assert!(runner.0.lock().unwrap().is_empty());
        guard.set_workspace(PathBuf::from("/work/mine"));
        assert!(auditors.oracle_for(&mine).is_known_git_object(sha));
        assert!(!auditors.oracle_for(&other).is_known_git_object(sha));
        assert_eq!(*runner.0.lock().unwrap(), vec![PathBuf::from("/work/mine")]);
        assert!(auditors.oracle_for(&mine).is_known_hub_id("mine"));
        assert!(!auditors.oracle_for(&mine).is_known_hub_id("ghost"));
    }
}
