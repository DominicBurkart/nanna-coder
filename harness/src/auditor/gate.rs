//! The only path to an [`Allowed`] proof: run a request through an
//! [`Auditor`], log the outcome, and let anything but `Allow` refuse it.

use super::{
    AuditContext, AuditError, AuditLog, AuditOutcome, AuditRecord, Auditor, CardSuggestion, Reason,
    ReasonCode, SpawnRequest, SpawnVerdict, VerdictKind,
};
use crate::identity::AgentIdentity;
use async_trait::async_trait;
use std::fmt;

/// Non-constructible proof that a [`SpawnRequest`] passed [`Gate::check`],
/// carrying the exact [`AgentIdentity`] the auditor consulted for it.
///
/// `Allowed` has no public constructor and no public fields, so the only way
/// to produce one is to hold a request an [`Auditor`] verdicted `Allow`
/// against an identity present in the same [`AuditContext`]'s catalog. A
/// spawn entry point that requires an `Allowed`, like
/// [`TaskManager::submit_spawn`](crate::task::TaskManager::submit_spawn), is
/// therefore statically guaranteed to run only audited requests under the
/// identity they were audited against: neither the caller nor the model
/// that proposed the spawn can manufacture one, or substitute a different
/// catalog's copy of the identity.
///
/// `Allowed`'s fields are private, so a spawn cannot be authorized by
/// writing a struct literal directly:
///
/// ```compile_fail
/// use harness::auditor::Allowed;
///
/// let _ = Allowed { request: todo!(), identity: todo!() };
/// ```
#[derive(Debug)]
pub struct Allowed {
    request: SpawnRequest,
    identity: AgentIdentity,
}

impl Allowed {
    fn new(request: SpawnRequest, identity: AgentIdentity) -> Self {
        Self { request, identity }
    }

    /// The request that was allowed.
    pub fn request(&self) -> &SpawnRequest {
        &self.request
    }

    /// The identity the request was audited against.
    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }

    /// Consume the proof and take back the request and identity it certified.
    pub fn into_parts(self) -> (SpawnRequest, AgentIdentity) {
        (self.request, self.identity)
    }
}

/// Why [`Gate::check`] refused a spawn.
#[derive(Debug)]
pub enum Refused {
    /// The auditor returned `Block` or `Escalate`.
    Verdict {
        /// The refusing verdict.
        verdict: SpawnVerdict,
        /// How it was reached.
        record: AuditRecord,
    },
    /// The auditor itself could not produce a verdict. Treated as a refusal,
    /// never as an allow: an auditor that cannot decide must not let a spawn
    /// through.
    AuditFailed(AuditError),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Verdict { verdict, .. } => write!(f, "spawn refused: {verdict}"),
            Refused::AuditFailed(error) => write!(f, "spawn refused: audit failed: {error}"),
        }
    }
}

impl std::error::Error for Refused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Refused::Verdict { .. } => None,
            Refused::AuditFailed(error) => Some(error),
        }
    }
}

/// An `Escalate` verdict, packaged for a [`SpawnEscalationHook`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnEscalation {
    /// The spawn that could not be placed.
    pub request: SpawnRequest,
    /// Why no card fit.
    pub reasons: Vec<Reason>,
    /// What a fitting card would look like.
    pub suggested_identity_change: CardSuggestion,
    /// How the escalation was reached.
    pub record: AuditRecord,
}

/// Notified whenever [`Gate::check`] escalates a spawn.
///
/// The default is a no-op; the escalation lane (issue #656) wires the real
/// sink (a GitHub issue, a webhook) by implementing this trait.
#[async_trait]
pub trait SpawnEscalationHook: Send + Sync {
    /// Called once per escalation, after it has been logged.
    async fn on_escalate(&self, escalation: &SpawnEscalation);
}

/// [`SpawnEscalationHook`] that does nothing; the default for [`Gate::new`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoopEscalationHook;

#[async_trait]
impl SpawnEscalationHook for NoopEscalationHook {
    async fn on_escalate(&self, _escalation: &SpawnEscalation) {}
}

/// Runs a [`SpawnRequest`] through an [`Auditor`], logs the outcome, and
/// hands back proof of the result: [`Allowed`] on `Allow`, [`Refused`]
/// otherwise. This is the only function in the crate that can produce an
/// [`Allowed`].
///
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// use harness::auditor::{AuditContext, AuditLog, Gate, ReasonCode, RuleAuditor, SpawnRequest, TaskSummary};
/// use harness::effects::EffectClass;
/// use harness::identity::{AgentIdentity, DevLoop, IdentityCatalog};
///
/// let dir = tempfile::tempdir().unwrap();
/// let card = |name: &str, max_effect: &str, tools: &str| format!(
///     "[identity]\nname = \"{name}\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\n\
///      system_prompt = {{ inline = \"p\" }}\n\n[scope]\nrepos = []\npaths = [\"**\"]\n\
///      max_effect = \"{max_effect}\"\ntools = [{tools}]\n\n[limits]\nmax_iterations = 1\n\
///      max_wall_clock_secs = 1\nmax_concurrent = 1\n"
/// );
/// std::fs::write(dir.path().join("rust-implementer.toml"), card("rust-implementer", "repository", "\"read_file\"")).unwrap();
/// std::fs::write(dir.path().join("auditor.toml"), card("auditor", "none", "")).unwrap();
/// let catalog = IdentityCatalog::load(dir.path()).unwrap();
/// let context = AuditContext::new(catalog.clone(), catalog.get("auditor").unwrap().clone()).unwrap();
///
/// let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
///
/// let fitting = SpawnRequest {
///     parent_task: TaskSummary::new("t1", "Fix bug", "github.com/example/repo"),
///     identity: "rust-implementer".to_string(),
///     subtask: "Add a test.".to_string(),
///     dev_loop: DevLoop::Inner,
///     requested_effect: EffectClass::Workspace,
/// };
/// let allowed = gate.check(fitting.clone(), &context).await.unwrap();
/// assert_eq!(allowed.request(), &fitting);
/// assert_eq!(allowed.identity().name(), "rust-implementer");
///
/// let mismatched = SpawnRequest { dev_loop: DevLoop::Outer, ..fitting };
/// let refused = gate.check(mismatched, &context).await.unwrap_err();
/// assert!(matches!(refused, harness::auditor::Refused::Verdict { .. }));
///
/// assert_eq!(gate.log().entries().unwrap().len(), 2);
/// # }
/// ```
pub struct Gate<A: Auditor, H: SpawnEscalationHook = NoopEscalationHook> {
    auditor: A,
    log: AuditLog,
    hook: H,
}

impl<A: Auditor> Gate<A, NoopEscalationHook> {
    /// A gate over `auditor`, appending to `log`, with no escalation hook.
    pub fn new(auditor: A, log: AuditLog) -> Self {
        Self {
            auditor,
            log,
            hook: NoopEscalationHook,
        }
    }
}

impl<A: Auditor, H: SpawnEscalationHook> Gate<A, H> {
    /// A gate over `auditor` with an explicit escalation hook.
    pub fn with_hook(auditor: A, log: AuditLog, hook: H) -> Self {
        Self { auditor, log, hook }
    }

    /// The audit log this gate appends every verdict to.
    pub fn log(&self) -> &AuditLog {
        &self.log
    }

    /// Review `request`, log the outcome, and return proof of the result.
    ///
    /// Logging is not best-effort: a log write failure is a [`Refused`], not
    /// a silently unlogged `Allow`. An `Allow` for an identity absent from
    /// `context`'s catalog is downgraded to a block before it is logged or
    /// returned, so a custom [`Auditor`] that forgets to check catalog
    /// membership cannot produce a forgeable [`Allowed`].
    pub async fn check(
        &self,
        request: SpawnRequest,
        context: &AuditContext,
    ) -> Result<Allowed, Refused> {
        let audited = self.auditor.audit_spawn(&request, context).await;
        let outcome = audited.map_err(Refused::AuditFailed)?;
        let outcome = reconcile_with_catalog(&request, context, outcome);
        self.log
            .append(&request, &outcome)
            .map_err(Refused::AuditFailed)?;
        self.settle(request, context, outcome).await
    }

    async fn settle(
        &self,
        request: SpawnRequest,
        context: &AuditContext,
        outcome: AuditOutcome,
    ) -> Result<Allowed, Refused> {
        let AuditOutcome { verdict, record } = outcome;
        if verdict.is_allow() {
            let identity = identity_for(&request, context);
            return Ok(Allowed::new(request, identity));
        }
        if verdict.kind() == VerdictKind::Escalate {
            self.notify_escalation(&request, &verdict, &record).await;
        }
        Err(Refused::Verdict { verdict, record })
    }

    async fn notify_escalation(
        &self,
        request: &SpawnRequest,
        verdict: &SpawnVerdict,
        record: &AuditRecord,
    ) {
        let suggestion = match verdict.suggestion() {
            Some(suggestion) => suggestion,
            None => unreachable!("escalate always has a suggestion"),
        };
        let escalation = SpawnEscalation {
            request: request.clone(),
            reasons: verdict.reasons().to_vec(),
            suggested_identity_change: suggestion.clone(),
            record: record.clone(),
        };
        self.hook.on_escalate(&escalation).await;
    }
}

fn reconcile_with_catalog(
    request: &SpawnRequest,
    context: &AuditContext,
    outcome: AuditOutcome,
) -> AuditOutcome {
    let known = context.catalog().get(&request.identity).is_some();
    if !outcome.verdict.is_allow() || known {
        return outcome;
    }
    let detail = "the auditor allowed an identity absent from its own catalog";
    let reason = Reason::new(ReasonCode::UnknownIdentity, detail);
    AuditOutcome {
        verdict: SpawnVerdict::block(vec![reason]),
        record: outcome.record,
    }
}

fn identity_for(request: &SpawnRequest, context: &AuditContext) -> AgentIdentity {
    match context.catalog().get(&request.identity) {
        Some(identity) => identity.clone(),
        None => unreachable!("reconcile_with_catalog only allows a known identity through"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::llm::tests::MockProvider;
    use crate::auditor::request::tests::request;
    use crate::auditor::rules::tests::context;
    use crate::auditor::{ModelAuditor, RuleAuditor, VerdictKind};
    use crate::effects::EffectClass;
    use crate::identity::DevLoop;
    use std::error::Error as _;
    use std::sync::Mutex;

    fn fitting() -> SpawnRequest {
        request(
            "rust-implementer",
            "Add a test.",
            DevLoop::Inner,
            EffectClass::Workspace,
        )
    }

    #[tokio::test]
    async fn allow_produces_an_allowed_proof_and_logs_it() {
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let request = fitting();
        let allowed = gate.check(request.clone(), &context(true)).await.unwrap();
        assert_eq!(allowed.request(), &request);
        assert_eq!(allowed.identity().name(), "rust-implementer");
        let (into_request, into_identity) = allowed.into_parts();
        assert_eq!(into_request, request);
        assert_eq!(into_identity.name(), "rust-implementer");
        let entries = gate.log().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].verdict.is_allow());
    }

    #[tokio::test]
    async fn block_is_refused_and_logged() {
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let request = request(
            "rust-implementer",
            "Add a test.",
            DevLoop::Outer,
            EffectClass::Workspace,
        );
        let refused = gate.check(request, &context(true)).await.unwrap_err();
        match &refused {
            Refused::Verdict { verdict, record } => {
                assert_eq!(verdict.kind(), VerdictKind::Block);
                assert_eq!(record.model, crate::auditor::RULE_AUDITOR_NAME);
            }
            other => panic!("expected Verdict, got {other:?}"),
        }
        assert!(refused.to_string().starts_with("spawn refused: block:"));
        assert!(refused.source().is_none());
        assert_eq!(gate.log().entries().unwrap().len(), 1);
    }

    #[derive(Default)]
    struct RecordingHook {
        seen: Mutex<Vec<SpawnEscalation>>,
    }

    #[async_trait]
    impl SpawnEscalationHook for RecordingHook {
        async fn on_escalate(&self, escalation: &SpawnEscalation) {
            self.seen.lock().unwrap().push(escalation.clone());
        }
    }

    #[tokio::test]
    async fn escalate_is_refused_logged_and_calls_the_hook() {
        let hook = RecordingHook::default();
        let gate = Gate::with_hook(RuleAuditor::new(), AuditLog::in_memory(), hook);
        let request = request(
            "rust-implementer",
            "Roll the fix out to the sandbox.",
            DevLoop::Inner,
            EffectClass::Sandbox,
        );
        let refused = gate
            .check(request.clone(), &context(false))
            .await
            .unwrap_err();
        match refused {
            Refused::Verdict { verdict, .. } => assert_eq!(verdict.kind(), VerdictKind::Escalate),
            other => panic!("expected Verdict, got {other:?}"),
        }
        let seen = gate.hook.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].request, request);
        assert_eq!(
            seen[0].suggested_identity_change.name,
            "rust-implementer-sandbox"
        );
        assert_eq!(gate.log().entries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn noop_hook_does_nothing_and_new_uses_it() {
        NoopEscalationHook
            .on_escalate(&SpawnEscalation {
                request: fitting(),
                reasons: vec![],
                suggested_identity_change: crate::auditor::CardSuggestion::widen(
                    &crate::identity::example(),
                    DevLoop::Outer,
                    EffectClass::Sandbox,
                    "r",
                ),
                record: AuditRecord {
                    model: "m".to_string(),
                    prompt_hash: "h".to_string(),
                    rationale: "r".to_string(),
                },
            })
            .await;
        assert_eq!(NoopEscalationHook, NoopEscalationHook);
    }

    #[tokio::test]
    async fn audit_failure_is_refused_never_allowed() {
        let provider = MockProvider::replying(&[]);
        let gate = Gate::new(ModelAuditor::new(provider, "m"), AuditLog::in_memory());
        let request = fitting();
        let refused = gate.check(request, &context(true)).await.unwrap_err();
        assert!(matches!(refused, Refused::AuditFailed(_)));
        assert!(refused
            .to_string()
            .starts_with("spawn refused: audit failed:"));
        assert!(refused.source().is_some());
        assert_eq!(gate.log().entries().unwrap().len(), 0);
    }

    #[test]
    fn allowed_proof_has_no_public_constructor() {
        let request = fitting();
        let identity = crate::identity::example();
        let allowed = Allowed::new(request.clone(), identity.clone());
        assert_eq!(allowed.request(), &request);
        assert_eq!(allowed.identity().name(), identity.name());
        assert_eq!(allowed.into_parts(), (request, identity));
    }

    struct AlwaysAllows;

    #[async_trait]
    impl Auditor for AlwaysAllows {
        fn name(&self) -> &str {
            "always-allows"
        }

        async fn review_spawn(
            &self,
            _request: &SpawnRequest,
            _context: &AuditContext,
        ) -> Result<SpawnVerdict, AuditError> {
            Ok(SpawnVerdict::Allow)
        }
    }

    #[tokio::test]
    async fn allow_for_an_identity_absent_from_the_catalog_is_downgraded_to_a_block() {
        let gate = Gate::new(AlwaysAllows, AuditLog::in_memory());
        let request = request("ghost", "Anything.", DevLoop::Inner, EffectClass::None);
        let refused = gate.check(request, &context(true)).await.unwrap_err();
        match refused {
            Refused::Verdict { verdict, .. } => {
                assert_eq!(verdict.kind(), VerdictKind::Block);
                assert_eq!(
                    verdict.reasons()[0].code,
                    crate::auditor::ReasonCode::UnknownIdentity
                );
            }
            other => panic!("expected Verdict, got {other:?}"),
        }
        let entries = gate.log().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].verdict.is_allow());
    }

    #[tokio::test]
    async fn a_log_write_failure_refuses_even_an_allow() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_directory = dir.path().join("blocker");
        std::fs::write(&not_a_directory, b"x").unwrap();
        let log = AuditLog::file(not_a_directory.join("audit.jsonl"));
        let gate = Gate::new(RuleAuditor::new(), log);
        let refused = gate.check(fitting(), &context(true)).await.unwrap_err();
        assert!(matches!(refused, Refused::AuditFailed(AuditError::Log(_))));
    }
}
