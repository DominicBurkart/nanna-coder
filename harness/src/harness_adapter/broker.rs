//! Host-side capability broker: the single door through which a sandboxed
//! runtime acts. It is not wired to anything yet: no executor, socket server
//! or `TaskRunner` method calls it, so nothing here changes what nanna runs.
//!
//! A `Broker` is built from a [`ResolvedAgent`] and the `BoundWorkspace` that
//! agent was bound to by `ResolvedAgent::bind_workspace`. It owns the registry
//! that bound workspace built and calls `ToolRegistry::execute` for every
//! request, so it reimplements no gate: scope, path restrictions and protected
//! paths stay inside the registry's tools, and the production hold and the
//! per-action gate stay inside `ToolRegistry::execute`. It only decides which
//! requests reach the registry, maps the registry's errors onto what the
//! runtime and the executor must do, and records each call.
//!
//! The order inside one call is: halted check, served check (capability
//! granted and at most the policy ceiling), then `ToolRegistry::execute`, which
//! looks the tool up, applies the production hold to `Production` calls, runs
//! the action review for `Repository`-class and above calls and for
//! `run_command`, and finally the tool, which enforces path scope and protected
//! paths. Calls below `Repository` other than `run_command` are not reviewed by
//! the action gate.
//!
//! An unarmed `Broker` cannot serve. `Broker::arm` consumes it and returns an
//! `ArmedBroker`, the only type with `handle`, after checking the launch plan
//! against this broker and the tool set the started runtime reports. A failed
//! arm returns no `ArmedBroker`, so there is no half-armed state.
//!
//! # What the executor must do on a halt
//!
//! `ArmedBroker::handle` returns [`BrokerOutcome::Halt`] when the per-action
//! gate escalates a call or a production hold applies. The runtime is told only
//! that the task was halted. The executor, which does not exist yet, must on
//! the first `Halt`:
//!
//! 1. Stop writing to the runtime and kill the container; do not wait for pi to
//!    exit, it may never do so.
//! 2. Fail the task with `FailureDiagnostics.error_type`
//!    [`HaltReason::error_type`] (`StateError`, the label `AgentLoop` uses for
//!    an escalated action) and the message of [`HaltReason::message`].
//! 3. Surface `ArmedBroker::action_audit` as the result's `action_audit`, the
//!    way `AgentLoop` does for an escalation.
//!
//! `HaltReason::ProductionHeld` is unreachable under the default
//! [`BrokerPolicy`], which serves nothing above `Workspace`; it exists for the
//! day the policy is widened.
//!
//! `ArmedBroker::handle` serialises calls with an async mutex held from the
//! halted check to the end of the call, so a call that starts after an
//! escalation sees the halt and is refused, and no two calls are in flight at
//! once: every call, including one already queued when another escalates,
//! passes the halted check and then the action gate. The halted check still
//! precedes the review inside a single call, which is the call that
//! escalates. Slices 2 and 3 must keep one connection per task and read one
//! request at a time; widening the policy beyond `Workspace` additionally
//! needs the lease and window handling named in [`BrokerPolicy`].
//!
//! Error messages put on the wire are clipped to [`MAX_MESSAGE_BYTES`], and
//! U+2028 and U+2029 are escaped in every reply line, so a reader that splits
//! lines on them cannot be tricked into a second frame; the executor must
//! still frame on LF only.

use super::binding::BoundWorkspace;
use super::{verify_runtime_tools, CheckedPlan, HandshakeMismatch, ResolvedAgent, ScopeGrant};
use crate::action_auditor::{ActionAuditLogEntry, ActionDenied};
use crate::auditor::{content_hash, Reason};
use crate::effects::EffectClass;
use crate::identity::LimitsSection;
use crate::task::TaskId;
use crate::tools::{ToolError, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Largest tool output the broker will return, in bytes of its JSON text. It is
/// below the extension's reply cap so the reply envelope still fits. The tool
/// has already run when this is exceeded; the output is replaced, not undone.
/// ```
/// use harness::harness_adapter::MAX_OUTPUT_BYTES;
///
/// assert_eq!(MAX_OUTPUT_BYTES, 1_000_000);
/// ```
pub const MAX_OUTPUT_BYTES: usize = 1_000_000;

/// Largest request line the broker will parse, in bytes.
/// ```
/// use harness::harness_adapter::MAX_REQUEST_BYTES;
///
/// assert_eq!(MAX_REQUEST_BYTES, 1_048_576);
/// ```
pub const MAX_REQUEST_BYTES: usize = 1_048_576;

/// Largest message the broker puts in an error reply, in bytes before JSON
/// escaping. It is one seventh of [`MAX_OUTPUT_BYTES`] so the escaped text, at
/// worst six bytes per control character, leaves room for the reply envelope.
/// ```
/// use harness::harness_adapter::{MAX_MESSAGE_BYTES, MAX_OUTPUT_BYTES};
///
/// assert_eq!(MAX_MESSAGE_BYTES, MAX_OUTPUT_BYTES / 7);
/// ```
pub const MAX_MESSAGE_BYTES: usize = MAX_OUTPUT_BYTES / 7;

const HALT_ERROR_TYPE: &str = "StateError";
const HALT_NOTICE: &str = "the task was halted by the host";
const MAX_LOGGED_NAME: usize = 128;

/// Which effect classes the broker will run. The default is
/// [`EffectClass::Workspace`]. The per-action gate is on main, but the action
/// subject a task carries has no window, pull request or environment, so
/// `Sandbox` and `Production` calls stop at the missing lease context, and the
/// leases and windows are unhandled, so the policy must not be widened before
/// that is closed. It can only be widened inside the crate, by the change that
/// does so.
///
/// ```
/// use harness::effects::EffectClass;
/// use harness::harness_adapter::BrokerPolicy;
///
/// let policy = BrokerPolicy::default();
/// assert!(policy.admits(EffectClass::Workspace));
/// assert!(!policy.admits(EffectClass::Repository));
/// assert_eq!(policy.max_effect(), EffectClass::Workspace);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerPolicy {
    max_effect: EffectClass,
}

impl BrokerPolicy {
    pub(crate) const fn new(max_effect: EffectClass) -> Self {
        Self { max_effect }
    }

    /// Whether a capability of class `effect` may run.
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::harness_adapter::BrokerPolicy;
    ///
    /// assert!(BrokerPolicy::default().admits(EffectClass::None));
    /// assert!(!BrokerPolicy::default().admits(EffectClass::Ci));
    /// ```
    pub fn admits(&self, effect: EffectClass) -> bool {
        effect <= self.max_effect
    }

    /// The highest admitted class.
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::harness_adapter::BrokerPolicy;
    ///
    /// assert_eq!(BrokerPolicy::default().max_effect(), EffectClass::Workspace);
    /// ```
    pub fn max_effect(&self) -> EffectClass {
        self.max_effect
    }
}

impl Default for BrokerPolicy {
    fn default() -> Self {
        Self::new(EffectClass::Workspace)
    }
}

#[derive(Debug, Error)]
pub(crate) enum BrokerError {
    #[error("the bound workspace's identity does not match the resolved agent `{agent}`")]
    IdentityMismatch { agent: String },
    #[error("the bound workspace was not built for this agent's grant")]
    BoundWorkspaceMismatch,
}

#[derive(Debug, Error)]
pub(crate) enum ArmError {
    #[error("plan does not belong to this broker: {field} differs")]
    PlanMismatch { field: &'static str },
    #[error("plan exposes capabilities the broker does not serve: {names:?}")]
    ExposedNotServed { names: Vec<String> },
    #[error(transparent)]
    Handshake(#[from] HandshakeMismatch),
}

/// A capability call arriving over the broker socket, one JSON object per line.
///
/// ```
/// use harness::harness_adapter::BrokerRequest;
///
/// let request: BrokerRequest = serde_json::from_str(r#"{"tool":"read_file"}"#).unwrap();
/// assert_eq!(request.tool, "read_file");
/// assert!(request.args.is_null());
/// let with_args: BrokerRequest =
///     serde_json::from_str(r#"{"tool":"search","args":{"q":"x"}}"#).unwrap();
/// assert_eq!(with_args.args["q"], "x");
/// assert!(serde_json::from_str::<BrokerRequest>(r#"{"args":{}}"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BrokerRequest {
    /// Capability name.
    pub tool: String,
    /// Call arguments.
    #[serde(default)]
    pub args: Value,
}

/// The reply written back to the runtime, one JSON object per line.
///
/// ```
/// use harness::harness_adapter::{BrokerOutcome, ToolErrorKind};
///
/// let reply = BrokerOutcome::Output(serde_json::json!("done")).to_response();
/// assert_eq!(
///     serde_json::to_value(&reply).unwrap(),
///     serde_json::json!({"ok": true, "output": "done", "halt": false})
/// );
/// let refused = BrokerOutcome::ToolError {
///     kind: ToolErrorKind::NotGranted,
///     message: "no".into(),
/// }
/// .to_response();
/// assert_eq!(
///     serde_json::to_value(&refused).unwrap(),
///     serde_json::json!({"ok": false, "output": "no", "kind": "not_granted", "halt": false})
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrokerResponse {
    /// Whether the call succeeded.
    pub ok: bool,
    /// The tool output, or an error message.
    pub output: Value,
    /// Machine-readable error kind, absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolErrorKind>,
    /// Whether the executor must stop the task.
    pub halt: bool,
}

/// Why a call returned an error to the agent without stopping the task.
///
/// ```
/// use harness::harness_adapter::ToolErrorKind;
///
/// assert_eq!(serde_json::to_string(&ToolErrorKind::NotGranted).unwrap(), "\"not_granted\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    /// The task already halted; nothing more is served.
    TaskHalted,
    /// The capability was never granted to this agent.
    NotGranted,
    /// The request was not valid JSON of the expected shape, or too large.
    BadRequest,
    /// The identity's scope refused the call.
    ScopeDenied,
    /// The call touched a protected path.
    ProtectedPath,
    /// The path escaped the workspace.
    PathSecurity,
    /// The arguments were invalid.
    InvalidArguments,
    /// The tool is not registered.
    NotFound,
    /// The tool ran and failed.
    ExecutionFailed,
    /// The per-action gate blocked the call; the agent may try something else.
    ActionBlocked,
    /// The task's budget refused the call; the agent may try something cheaper.
    BudgetRefused,
    /// The tool's output exceeded [`MAX_OUTPUT_BYTES`].
    ReplyTooLarge,
}

/// Why the task must stop. The detail stays host-side: the runtime is only told
/// that the task was halted.
///
/// ```
/// use harness::harness_adapter::HaltReason;
///
/// let reason = HaltReason::ProductionHeld {
///     repo: "r".into(),
///     escalation_id: "e1".into(),
///     summary: "down".into(),
/// };
/// assert_eq!(reason.error_type(), "StateError");
/// assert_eq!(reason.message(), "production hold e1 on r halted the task");
///
/// let escalated = HaltReason::ActionEscalated {
///     tool: "run_command".into(),
///     reasons: vec![],
/// };
/// assert_eq!(escalated.error_type(), "StateError");
/// assert_eq!(
///     escalated.message(),
///     "action review escalated `run_command`; see action_audit in the task result"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaltReason {
    /// An incident hold blocks production effects.
    ProductionHeld {
        /// Repository under hold.
        repo: String,
        /// Escalation that set the hold.
        escalation_id: String,
        /// Human summary of the incident.
        summary: String,
    },
    /// The per-action gate escalated the call for human review. The registry
    /// has already delivered the escalation through its escalator.
    ActionEscalated {
        /// The escalated capability, escaped and truncated.
        tool: String,
        /// The gate's findings.
        reasons: Vec<Reason>,
    },
}

impl HaltReason {
    /// The `FailureDiagnostics.error_type` the executor fails the task with:
    /// the label `AgentLoop` uses when an action review escalates.
    ///
    /// ```
    /// use harness::harness_adapter::HaltReason;
    ///
    /// let reason = HaltReason::ActionEscalated { tool: "t".into(), reasons: vec![] };
    /// assert_eq!(reason.error_type(), "StateError");
    /// ```
    pub fn error_type(&self) -> &'static str {
        HALT_ERROR_TYPE
    }

    /// The failure message for the task result. It names the tool or the hold
    /// and points at `action_audit`; the findings and the incident summary are
    /// not included.
    ///
    /// ```
    /// use harness::harness_adapter::HaltReason;
    ///
    /// let reason = HaltReason::ActionEscalated { tool: "run_command".into(), reasons: vec![] };
    /// assert!(reason.message().starts_with("action review escalated `run_command`"));
    /// ```
    pub fn message(&self) -> String {
        match self {
            HaltReason::ProductionHeld {
                repo,
                escalation_id,
                ..
            } => format!("production hold {escalation_id} on {repo} halted the task"),
            HaltReason::ActionEscalated { tool, .. } => {
                format!("action review escalated `{tool}`; see action_audit in the task result")
            }
        }
    }
}

/// What the broker decided for one request.
///
/// ```
/// use harness::harness_adapter::BrokerOutcome;
///
/// let output = BrokerOutcome::Output(serde_json::json!("done"));
/// assert!(!output.is_halt());
/// let reply = output.to_response();
/// assert!(reply.ok && !reply.halt);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum BrokerOutcome {
    /// The tool ran; its output.
    Output(Value),
    /// An error returned to the agent, which may adapt and continue.
    ToolError {
        /// Machine-readable kind.
        kind: ToolErrorKind,
        /// Human-readable message.
        message: String,
    },
    /// The task must stop; the executor aborts the runtime.
    Halt(HaltReason),
}

impl BrokerOutcome {
    /// Whether the executor must stop the task.
    ///
    /// ```
    /// use harness::harness_adapter::{BrokerOutcome, HaltReason};
    ///
    /// let halt = BrokerOutcome::Halt(HaltReason::ActionEscalated {
    ///     tool: "t".into(),
    ///     reasons: vec![],
    /// });
    /// assert!(halt.is_halt());
    /// ```
    pub fn is_halt(&self) -> bool {
        matches!(self, BrokerOutcome::Halt(_))
    }

    /// The wire reply for this outcome. A halt carries a fixed notice only.
    ///
    /// ```
    /// use harness::harness_adapter::{BrokerOutcome, HaltReason};
    ///
    /// let halt = BrokerOutcome::Halt(HaltReason::ProductionHeld {
    ///     repo: "r".into(),
    ///     escalation_id: "e1".into(),
    ///     summary: "down".into(),
    /// });
    /// let reply = halt.to_response();
    /// assert!(reply.halt && !reply.ok && reply.kind.is_none());
    /// assert_eq!(reply.output, "the task was halted by the host");
    /// assert!(!reply.output.to_string().contains("down"));
    /// ```
    pub fn to_response(&self) -> BrokerResponse {
        match self {
            BrokerOutcome::Output(output) => BrokerResponse {
                ok: true,
                output: output.clone(),
                kind: None,
                halt: false,
            },
            BrokerOutcome::ToolError { kind, message } => BrokerResponse {
                ok: false,
                output: json!(clip(message, MAX_MESSAGE_BYTES)),
                kind: Some(*kind),
                halt: false,
            },
            BrokerOutcome::Halt(_) => BrokerResponse {
                ok: false,
                output: json!(HALT_NOTICE),
                kind: None,
                halt: true,
            },
        }
    }
}

/// How a recorded call ended.
///
/// ```
/// use harness::harness_adapter::{AuditOutcome, ToolErrorKind};
///
/// assert_ne!(AuditOutcome::Ok, AuditOutcome::Error(ToolErrorKind::NotGranted));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum AuditOutcome {
    /// The tool ran successfully.
    Ok,
    /// The call returned an error to the agent.
    Error(ToolErrorKind),
    /// The call stopped the task.
    Halt,
    /// The call never finished: the tool panicked or the call was dropped.
    Aborted,
}

/// One broker decision. It holds no argument content: only the size and a
/// non-cryptographic fingerprint, so the log cannot leak secrets or file
/// contents and cannot grow with argument size.
///
/// ```
/// use harness::harness_adapter::{AuditOutcome, CallRecord};
///
/// let record = CallRecord {
///     started: 0,
///     completed: 0,
///     tool: "read_file".into(),
///     args_bytes: 2,
///     args_fingerprint: "00".into(),
///     effect: None,
///     outcome: AuditOutcome::Ok,
/// };
/// assert_eq!(record.tool, "read_file");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    /// Sequence number assigned when the call started: this is call order.
    pub started: u64,
    /// Sequence number assigned when the call finished: this is completion order.
    pub completed: u64,
    /// The requested name, escaped and truncated.
    pub tool: String,
    /// Size in bytes of the compact JSON of the arguments.
    pub args_bytes: usize,
    /// FNV-1a fingerprint of that JSON, for correlation only. It is not a
    /// cryptographic digest and must not be relied on to hide low-entropy secrets.
    pub args_fingerprint: String,
    /// Effect class when the capability is one the agent was granted.
    pub effect: Option<EffectClass>,
    /// How the call ended.
    pub outcome: AuditOutcome,
}

/// Where the broker records each call. The executor injects the sink; a
/// durable one is the executor slice's job. The existing `auditor::AuditLog`
/// records spawn reviews (a `SpawnRequest` and verdict), not tool calls, so it
/// does not fit and no bridge is attempted here.
///
/// ```
/// use harness::harness_adapter::{AuditOutcome, AuditSink, CallRecord};
/// use std::sync::Mutex;
///
/// struct Collect(Mutex<Vec<CallRecord>>);
///
/// impl AuditSink for Collect {
///     fn record(&self, record: CallRecord) {
///         self.0.lock().unwrap().push(record);
///     }
/// }
///
/// let sink = Collect(Mutex::new(Vec::new()));
/// sink.record(CallRecord {
///     started: 0,
///     completed: 0,
///     tool: "read_file".into(),
///     args_bytes: 2,
///     args_fingerprint: "00".into(),
///     effect: None,
///     outcome: AuditOutcome::Ok,
/// });
/// assert_eq!(sink.0.lock().unwrap().len(), 1);
/// ```
pub trait AuditSink: Send + Sync {
    /// Record one finished call.
    fn record(&self, record: CallRecord);
}

/// A bounded in-memory [`AuditSink`]: when full it drops the oldest record and
/// counts the drop.
///
/// ```
/// use harness::harness_adapter::{AuditOutcome, AuditSink, CallRecord, RingAuditSink};
///
/// let record = |n: u64| CallRecord {
///     started: n,
///     completed: n,
///     tool: format!("t{n}"),
///     args_bytes: 0,
///     args_fingerprint: String::new(),
///     effect: None,
///     outcome: AuditOutcome::Ok,
/// };
/// let sink = RingAuditSink::new(2);
/// for n in 0..3 {
///     sink.record(record(n));
/// }
/// assert_eq!(sink.dropped(), 1);
/// assert_eq!(sink.entries().len(), 2);
/// assert_eq!(RingAuditSink::default().dropped(), 0);
/// ```
#[derive(Debug)]
pub struct RingAuditSink {
    capacity: usize,
    state: Mutex<(VecDeque<CallRecord>, u64)>,
}

impl RingAuditSink {
    /// A sink keeping the most recent `capacity` records (at least one).
    ///
    /// ```
    /// use harness::harness_adapter::{AuditOutcome, AuditSink, CallRecord, RingAuditSink};
    ///
    /// let sink = RingAuditSink::new(0);
    /// let record = CallRecord {
    ///     started: 0,
    ///     completed: 0,
    ///     tool: "t".into(),
    ///     args_bytes: 0,
    ///     args_fingerprint: String::new(),
    ///     effect: None,
    ///     outcome: AuditOutcome::Ok,
    /// };
    /// sink.record(record.clone());
    /// sink.record(record);
    /// assert_eq!(sink.entries().len(), 1);
    /// assert_eq!(sink.dropped(), 1);
    /// ```
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new((VecDeque::new(), 0)),
        }
    }

    /// The retained records, oldest first.
    ///
    /// ```
    /// use harness::harness_adapter::RingAuditSink;
    ///
    /// assert!(RingAuditSink::default().entries().is_empty());
    /// ```
    pub fn entries(&self) -> Vec<CallRecord> {
        let state = self.state.lock().expect("audit sink poisoned");
        state.0.iter().cloned().collect()
    }

    /// How many records were dropped because the sink was full.
    ///
    /// ```
    /// use harness::harness_adapter::RingAuditSink;
    ///
    /// assert_eq!(RingAuditSink::new(2).dropped(), 0);
    /// ```
    pub fn dropped(&self) -> u64 {
        self.state.lock().expect("audit sink poisoned").1
    }
}

impl Default for RingAuditSink {
    fn default() -> Self {
        Self::new(10_000)
    }
}

impl AuditSink for RingAuditSink {
    fn record(&self, record: CallRecord) {
        let mut state = self.state.lock().expect("audit sink poisoned");
        if state.0.len() >= self.capacity {
            state.0.pop_front();
            state.1 += 1;
        }
        state.0.push_back(record);
    }
}

struct Core {
    agent: String,
    audited_repo: String,
    model: String,
    image: String,
    limits: LimitsSection,
    scope: ScopeGrant,
    broker_socket: std::path::PathBuf,
    capabilities: Vec<String>,
    task_id: TaskId,
    registry: ToolRegistry,
    served: BTreeSet<String>,
    effects: Vec<(String, EffectClass)>,
    sink: Arc<dyn AuditSink>,
    started: AtomicU64,
    completed: AtomicU64,
    halted: AtomicBool,
    serial: tokio::sync::Mutex<()>,
}

pub(crate) struct Broker {
    core: Core,
}

pub(crate) struct ArmedBroker {
    core: Core,
}

impl Broker {
    pub(crate) fn for_agent(
        agent: &ResolvedAgent,
        bound: BoundWorkspace,
        policy: BrokerPolicy,
        sink: Arc<dyn AuditSink>,
    ) -> Result<Self, BrokerError> {
        if !bound.agent_matches(agent) {
            return Err(BrokerError::IdentityMismatch {
                agent: agent.name().to_string(),
            });
        }
        if !bound.capabilities_match(agent) {
            return Err(BrokerError::BoundWorkspaceMismatch);
        }
        let mut served = BTreeSet::new();
        let mut effects = Vec::new();
        for capability in agent.capabilities() {
            effects.push((capability.name.clone(), capability.effect));
            if policy.admits(capability.effect) {
                served.insert(capability.name.clone());
            }
        }
        Ok(Self {
            core: Core {
                agent: agent.name().to_string(),
                audited_repo: agent.audited_repo().to_string(),
                model: agent.model().to_string(),
                image: agent.image().to_string(),
                limits: agent.limits(),
                scope: agent.scope().clone(),
                broker_socket: agent.broker_socket().to_path_buf(),
                capabilities: agent
                    .capability_names()
                    .into_iter()
                    .map(String::from)
                    .collect(),
                task_id: bound.task_id().clone(),
                registry: bound.into_registry(),
                served,
                effects,
                sink,
                started: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                halted: AtomicBool::new(false),
                serial: tokio::sync::Mutex::new(()),
            },
        })
    }

    #[cfg(test)]
    pub(super) fn installed_layers(&self) -> crate::tools::InstalledLayers {
        self.core.registry.installed_layers()
    }

    pub(crate) fn served(&self) -> Vec<&str> {
        self.core.served.iter().map(String::as_str).collect()
    }

    pub(crate) fn task_id(&self) -> &TaskId {
        &self.core.task_id
    }

    pub(crate) fn arm(
        self,
        plan: &CheckedPlan,
        reported: &[String],
    ) -> Result<ArmedBroker, ArmError> {
        let core = &self.core;
        let checks: [(&'static str, bool); 8] = [
            ("agent", plan.agent == core.agent),
            ("audited repository", plan.audited_repo == core.audited_repo),
            ("model", plan.model == core.model),
            ("image", plan.image == core.image),
            ("limits", plan.limits == core.limits),
            ("scope", plan.scope == core.scope),
            ("broker socket", plan.broker_socket == core.broker_socket),
            (
                "capabilities",
                plan.exposed_capabilities == core.capabilities,
            ),
        ];
        if let Some((field, _)) = checks.iter().find(|(_, same)| !same) {
            return Err(ArmError::PlanMismatch { field });
        }
        let unserved: Vec<String> = plan
            .exposed_capabilities
            .iter()
            .filter(|name| !core.served.contains(name.as_str()))
            .cloned()
            .collect();
        if !unserved.is_empty() {
            return Err(ArmError::ExposedNotServed { names: unserved });
        }
        verify_runtime_tools(plan, reported)?;
        Ok(ArmedBroker { core: self.core })
    }
}

impl ArmedBroker {
    #[cfg(test)]
    pub(super) fn installed_layers(&self) -> crate::tools::InstalledLayers {
        self.core.registry.installed_layers()
    }

    pub(crate) fn is_halted(&self) -> bool {
        self.core.halted.load(Ordering::SeqCst)
    }

    pub(crate) fn task_id(&self) -> &TaskId {
        &self.core.task_id
    }

    pub(crate) fn action_audit(&self) -> Vec<ActionAuditLogEntry> {
        self.core.registry.action_reviews()
    }

    pub(crate) async fn handle(&self, request: BrokerRequest) -> BrokerOutcome {
        let core = &self.core;
        let _serial = core.serial.lock().await;
        let args_text = request.args.to_string();
        let mut guard = CallGuard {
            core,
            record: Some(CallRecord {
                started: core.started.fetch_add(1, Ordering::SeqCst),
                completed: 0,
                tool: logged_name(&request.tool),
                args_bytes: args_text.len(),
                args_fingerprint: content_hash(&args_text),
                effect: core
                    .effects
                    .iter()
                    .find(|(name, _)| *name == request.tool)
                    .map(|(_, effect)| *effect),
                outcome: AuditOutcome::Aborted,
            }),
        };
        let outcome = self.decide(&request).await;
        guard.finish(match &outcome {
            BrokerOutcome::Output(_) => AuditOutcome::Ok,
            BrokerOutcome::ToolError { kind, .. } => AuditOutcome::Error(*kind),
            BrokerOutcome::Halt(_) => AuditOutcome::Halt,
        });
        outcome
    }

    async fn decide(&self, request: &BrokerRequest) -> BrokerOutcome {
        let core = &self.core;
        let refuse = |kind, message: String| BrokerOutcome::ToolError { kind, message };
        if self.is_halted() {
            return refuse(ToolErrorKind::TaskHalted, "the task has halted".into());
        }
        if !core.served.contains(&request.tool) {
            return refuse(
                ToolErrorKind::NotGranted,
                format!("capability `{}` is not granted", logged_name(&request.tool)),
            );
        }
        let result = core
            .registry
            .execute(&request.tool, request.args.clone())
            .await;
        let outcome = map_result(result, &request.tool);
        if outcome.is_halt() {
            core.halted.store(true, Ordering::SeqCst);
        }
        match outcome {
            BrokerOutcome::Output(value)
                if wire_text(&value.to_string()).len() > MAX_OUTPUT_BYTES =>
            {
                refuse(
                    ToolErrorKind::ReplyTooLarge,
                    format!(
                        "output of `{}` exceeds {MAX_OUTPUT_BYTES} bytes",
                        request.tool
                    ),
                )
            }
            outcome => outcome,
        }
    }

    pub(crate) async fn handle_line(&self, line: &str) -> String {
        let bad = |message: String| BrokerResponse {
            ok: false,
            output: json!(clip(&message, MAX_MESSAGE_BYTES)),
            kind: Some(ToolErrorKind::BadRequest),
            halt: false,
        };
        let response = if line.len() > MAX_REQUEST_BYTES {
            bad("request too large".to_string())
        } else {
            match serde_json::from_str::<BrokerRequest>(line) {
                Ok(request) => self.handle(request).await.to_response(),
                Err(error) => bad(format!("bad request: {error}")),
            }
        };
        let text = serde_json::to_string(&response).expect("broker responses are plain JSON");
        wire_text(&text)
    }
}

struct CallGuard<'a> {
    core: &'a Core,
    record: Option<CallRecord>,
}

impl CallGuard<'_> {
    fn finish(&mut self, outcome: AuditOutcome) {
        if let Some(mut record) = self.record.take() {
            record.outcome = outcome;
            record.completed = self.core.completed.fetch_add(1, Ordering::SeqCst);
            self.core.sink.record(record);
        }
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.finish(AuditOutcome::Aborted);
    }
}

fn wire_text(text: &str) -> String {
    let text = text.replace('\u{2028}', "\\u2028");
    text.replace('\u{2029}', "\\u2029")
}

pub(super) fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn logged_name(name: &str) -> String {
    let escaped: String = name.escape_debug().collect();
    let mut end = 0;
    for (index, ch) in escaped.char_indices() {
        let next = index + ch.len_utf8();
        if next > MAX_LOGGED_NAME {
            break;
        }
        end = next;
    }
    escaped[..end].to_string()
}

fn map_result(result: Result<Value, ToolError>, tool: &str) -> BrokerOutcome {
    let (kind, error) = match result {
        Ok(value) => return BrokerOutcome::Output(value),
        Err(ToolError::ProductionHeld {
            repo,
            escalation_id,
            summary,
        }) => {
            return BrokerOutcome::Halt(HaltReason::ProductionHeld {
                repo,
                escalation_id,
                summary,
            })
        }
        Err(ToolError::ActionDenied(ActionDenied::Escalate { reasons })) => {
            return BrokerOutcome::Halt(HaltReason::ActionEscalated {
                tool: logged_name(tool),
                reasons,
            })
        }
        Err(error @ ToolError::ActionDenied(ActionDenied::Block { .. })) => {
            (ToolErrorKind::ActionBlocked, error)
        }
        Err(error @ ToolError::InvalidArguments { .. }) => (ToolErrorKind::InvalidArguments, error),
        Err(error @ ToolError::ExecutionFailed { .. }) => (ToolErrorKind::ExecutionFailed, error),
        Err(error @ ToolError::NotFound { .. }) => (ToolErrorKind::NotFound, error),
        Err(error @ ToolError::Serialization(_)) => (ToolErrorKind::ExecutionFailed, error),
        Err(error @ ToolError::PathSecurityViolation { .. }) => {
            (ToolErrorKind::PathSecurity, error)
        }
        Err(error @ ToolError::ScopeDenied(_)) => (ToolErrorKind::ScopeDenied, error),
        Err(error @ ToolError::ProtectedPath(_)) => (ToolErrorKind::ProtectedPath, error),
        Err(error @ ToolError::Io(_)) => (ToolErrorKind::ExecutionFailed, error),
        Err(error @ ToolError::Budget(_)) => (ToolErrorKind::BudgetRefused, error),
    };
    BrokerOutcome::ToolError {
        kind,
        message: error.to_string(),
    }
}
