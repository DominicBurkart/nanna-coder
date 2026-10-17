//! Host-side capability broker: the single door through which a sandboxed
//! runtime acts.
//!
//! A [`Broker`] is built from the audit proof, a [`ResolvedAgent`] and the
//! [`BoundWorkspace`] that agent was bound to. It owns the registry that bound
//! workspace built and calls [`ToolRegistry::execute`] for every request, so it
//! reimplements no gate: scope, path restrictions, protected paths and the
//! production hold stay inside the registry and its tools. It only decides
//! which requests reach the registry, maps the registry's errors onto what the
//! runtime and the executor must do, and records each call.
//!
//! An unarmed [`Broker`] cannot serve. [`Broker::arm`] consumes it and returns
//! an [`ArmedBroker`], the only type with `handle`, after checking the launch
//! plan against this broker and the tool set the started runtime reports. A
//! failed arm returns no [`ArmedBroker`], so there is no half-armed state.
//!
//! ```compile_fail
//! use harness::harness_adapter::{Broker, BrokerRequest};
//!
//! async fn unarmed_cannot_serve(broker: Broker, request: BrokerRequest) {
//!     broker.handle(request).await;
//! }
//! ```

use super::bind::BoundWorkspace;
use super::{verify_runtime_tools, CheckedPlan, HandshakeMismatch, ResolvedAgent, ScopeGrant};
use crate::auditor::{content_hash, Allowed};
use crate::effects::EffectClass;
use crate::identity::LimitsSection;
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

const HALT_NOTICE: &str = "the task was halted by the host";
const MAX_LOGGED_NAME: usize = 128;

/// Which effect classes the broker will run. The default is
/// [`EffectClass::Workspace`]: the per-action gate (window, lease, ceiling,
/// escalation) is not on main yet, so anything above Workspace would run with
/// no window or lease check. It can only be widened inside the crate, by the
/// change that adds that gate.
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

/// Failures building a [`Broker`].
///
/// ```
/// use harness::harness_adapter::BrokerError;
///
/// let error = BrokerError::IdentityMismatch { identity: "a".into(), agent: "b".into() };
/// assert!(error.to_string().contains("does not match the resolved agent"));
/// ```
#[derive(Debug, Error)]
pub enum BrokerError {
    /// The audited spawn is not the one the agent was resolved from.
    #[error("audited identity `{identity}` does not match the resolved agent `{agent}`")]
    IdentityMismatch {
        /// Identity from the audit proof.
        identity: String,
        /// Agent the broker was built for.
        agent: String,
    },
}

/// Why a broker refused to arm.
///
/// ```
/// use harness::harness_adapter::ArmError;
///
/// let error = ArmError::PlanMismatch { field: "model" };
/// assert_eq!(error.to_string(), "plan does not belong to this broker: model differs");
/// ```
#[derive(Debug, Error)]
pub enum ArmError {
    /// The plan was built for a different agent, repository, model, image,
    /// limits, scope, capability set or broker socket than this broker.
    #[error("plan does not belong to this broker: {field} differs")]
    PlanMismatch {
        /// The field that differs.
        field: &'static str,
    },
    /// The plan exposes capabilities this broker would refuse.
    #[error("plan exposes capabilities the broker does not serve: {names:?}")]
    ExposedNotServed {
        /// The capabilities that are exposed but not served.
        names: Vec<String>,
    },
    /// The started runtime reports a tool set different from the plan.
    #[error(transparent)]
    Handshake(#[from] HandshakeMismatch),
}

/// A capability call arriving over the broker socket, one JSON object per line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
/// ```
/// use harness::harness_adapter::BrokerRequest;
///
/// let request: BrokerRequest = serde_json::from_str(r#"{"tool":"read_file"}"#).unwrap();
/// assert_eq!(request.tool, "read_file");
/// assert!(request.args.is_null());
/// ```
pub struct BrokerRequest {
    /// Capability name.
    pub tool: String,
    /// Call arguments.
    #[serde(default)]
    pub args: Value,
}

/// The reply written back to the runtime, one JSON object per line.
#[derive(Debug, Clone, PartialEq, Serialize)]
/// ```
/// use harness::harness_adapter::BrokerOutcome;
///
/// let reply = BrokerOutcome::Output(serde_json::json!("done")).to_response();
/// assert_eq!(
///     serde_json::to_value(&reply).unwrap(),
///     serde_json::json!({"ok": true, "output": "done", "halt": false})
/// );
/// ```
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
/// assert!(matches!(reason, HaltReason::ProductionHeld { .. }));
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
}

/// What the broker decided for one request.
///
/// ```
/// use harness::harness_adapter::BrokerOutcome;
///
/// let reply = BrokerOutcome::Output(serde_json::json!("done")).to_response();
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
    /// The wire reply for this outcome. A halt carries a fixed notice only.
    /// ```
    /// use harness::harness_adapter::{BrokerOutcome, HaltReason};
    ///
    /// let halt = BrokerOutcome::Halt(HaltReason::ProductionHeld {
    ///     repo: "r".into(),
    ///     escalation_id: "e1".into(),
    ///     summary: "down".into(),
    /// });
    /// let reply = halt.to_response();
    /// assert!(reply.halt && !reply.ok);
    /// assert_eq!(reply.output, "the task was halted by the host");
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
                output: json!(message),
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
/// use harness::harness_adapter::{AuditSink, RingAuditSink};
///
/// let sink = RingAuditSink::new(1);
/// assert!(sink.entries().is_empty());
/// ```
pub trait AuditSink: Send + Sync {
    /// Record one finished call.
    fn record(&self, record: CallRecord);
}

/// A bounded in-memory [`AuditSink`]: when full it drops the oldest record and
/// counts the drop.
///
/// ```
/// use harness::harness_adapter::RingAuditSink;
///
/// let sink = RingAuditSink::default();
/// assert_eq!(sink.dropped(), 0);
/// ```
#[derive(Debug)]
pub struct RingAuditSink {
    capacity: usize,
    state: Mutex<(VecDeque<CallRecord>, u64)>,
}

impl RingAuditSink {
    /// A sink keeping the most recent `capacity` records (at least one).
    /// ```
    /// use harness::harness_adapter::RingAuditSink;
    ///
    /// let sink = RingAuditSink::new(0);
    /// assert!(sink.entries().is_empty());
    /// ```
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new((VecDeque::new(), 0)),
        }
    }

    /// The retained records, oldest first.
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
    registry: ToolRegistry,
    served: BTreeSet<String>,
    effects: Vec<(String, EffectClass)>,
    sink: Arc<dyn AuditSink>,
    started: AtomicU64,
    completed: AtomicU64,
    halted: AtomicBool,
}

/// A broker that has not been armed. It cannot serve; see [`Broker::arm`].
/// ```
/// use harness::harness_adapter::{ArmedBroker, ArmError, Broker, CheckedPlan};
///
/// fn arm(broker: Broker, plan: &CheckedPlan, reported: &[String]) -> Result<ArmedBroker, ArmError> {
///     broker.arm(plan, reported)
/// }
/// ```
pub struct Broker {
    core: Core,
}

/// An armed broker: the only type that serves capability calls.
/// ```compile_fail
/// use harness::harness_adapter::ArmedBroker;
///
/// fn forge() -> ArmedBroker {
///     ArmedBroker { core: todo!() }
/// }
/// ```
pub struct ArmedBroker {
    core: Core,
}

impl Broker {
    /// Build the broker for `agent`. The identity comes from the audit proof
    /// `allowed`, never from the caller, and `bound` is the registry built for
    /// that audited spawn by [`ResolvedAgent::bind_workspace`]; there is no way
    /// to supply a registry built elsewhere. `agent` must be the one resolved
    /// from `allowed`. Capabilities above `policy` are not served, and a plan
    /// that exposes one cannot arm. Every call is recorded in `sink`.
    /// ```
    /// use std::sync::Arc;
    ///
    /// use harness::auditor::Allowed;
    /// use harness::harness_adapter::{
    ///     BoundWorkspace, Broker, BrokerError, BrokerPolicy, ResolvedAgent, RingAuditSink,
    /// };
    ///
    /// fn build(
    ///     allowed: &Allowed,
    ///     agent: &ResolvedAgent,
    ///     bound: BoundWorkspace,
    /// ) -> Result<Broker, BrokerError> {
    ///     let sink = Arc::new(RingAuditSink::default());
    ///     Broker::for_agent(allowed, agent, bound, BrokerPolicy::default(), sink)
    /// }
    /// ```
    pub fn for_agent(
        allowed: &Allowed,
        agent: &ResolvedAgent,
        bound: BoundWorkspace,
        policy: BrokerPolicy,
        sink: Arc<dyn AuditSink>,
    ) -> Result<Self, BrokerError> {
        let identity = allowed.identity();
        let scope = agent.scope();
        let same = agent.name() == identity.name()
            && scope.repos == identity.scope.repos
            && scope.paths == identity.scope.paths
            && scope.read_paths == identity.scope.read_paths
            && agent.max_effect() == identity.scope.max_effect
            && agent.audited_repo() == allowed.repo();
        if !same {
            return Err(BrokerError::IdentityMismatch {
                identity: identity.name().to_string(),
                agent: agent.name().to_string(),
            });
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
                registry: bound.into_registry(),
                served,
                effects,
                sink,
                started: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                halted: AtomicBool::new(false),
            },
        })
    }

    /// Names the broker will run, sorted.
    /// ```
    /// use harness::harness_adapter::Broker;
    ///
    /// fn served(broker: &Broker) -> Vec<&str> {
    ///     broker.served()
    /// }
    /// ```
    pub fn served(&self) -> Vec<&str> {
        self.core.served.iter().map(String::as_str).collect()
    }

    /// Arm the broker. The executor calls this once the runtime has started and
    /// reported its tools (for pi, the `toolsAdded` list in the first system
    /// `message_start`). It consumes the broker: on success it returns the
    /// [`ArmedBroker`], on failure nothing, so no state survives a rejected
    /// arm. It checks that `plan` names this agent, audited repository, model,
    /// image, limits, scope, capability set and broker socket, that every
    /// exposed capability is one the broker serves, and that the runtime's
    /// reported tools equal the plan's. The model already holds its tools before
    /// the check, so nothing is served until it passes.
    /// ```
    /// use harness::harness_adapter::{ArmedBroker, ArmError, Broker, CheckedPlan};
    ///
    /// fn arm(broker: Broker, plan: &CheckedPlan, reported: &[String]) -> Result<ArmedBroker, ArmError> {
    ///     broker.arm(plan, reported)
    /// }
    /// ```
    pub fn arm(self, plan: &CheckedPlan, reported: &[String]) -> Result<ArmedBroker, ArmError> {
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
    /// Whether the task has halted.
    /// ```
    /// use harness::harness_adapter::ArmedBroker;
    ///
    /// fn stopped(broker: &ArmedBroker) -> bool {
    ///     broker.is_halted()
    /// }
    /// ```
    pub fn is_halted(&self) -> bool {
        self.core.halted.load(Ordering::SeqCst)
    }

    /// Handle one request. The halt flag is set inside the decision, the moment
    /// the registry returns the hold; calls already running when it is set
    /// complete.
    /// ```
    /// use harness::harness_adapter::{ArmedBroker, BrokerOutcome, BrokerRequest};
    ///
    /// async fn call(broker: &ArmedBroker, request: BrokerRequest) -> BrokerOutcome {
    ///     broker.handle(request).await
    /// }
    /// ```
    pub async fn handle(&self, request: BrokerRequest) -> BrokerOutcome {
        let core = &self.core;
        let started = core.started.fetch_add(1, Ordering::SeqCst);
        let effect = core
            .effects
            .iter()
            .find(|(name, _)| *name == request.tool)
            .map(|(_, effect)| *effect);
        let args_text = request.args.to_string();
        let outcome = self.decide(&request).await;
        let outcome_kind = match &outcome {
            BrokerOutcome::Output(_) => AuditOutcome::Ok,
            BrokerOutcome::ToolError { kind, .. } => AuditOutcome::Error(*kind),
            BrokerOutcome::Halt(_) => AuditOutcome::Halt,
        };
        let mut tool: String = request.tool.escape_debug().collect();
        tool.truncate(MAX_LOGGED_NAME);
        core.sink.record(CallRecord {
            started,
            completed: core.completed.fetch_add(1, Ordering::SeqCst),
            tool,
            args_bytes: args_text.len(),
            args_fingerprint: content_hash(&args_text),
            effect,
            outcome: outcome_kind,
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
                format!("capability `{}` is not granted", request.tool),
            );
        }
        let result = core
            .registry
            .execute(&request.tool, request.args.clone())
            .await;
        let outcome = map_result(result);
        if matches!(outcome, BrokerOutcome::Halt(_)) {
            core.halted.store(true, Ordering::SeqCst);
        }
        match outcome {
            BrokerOutcome::Output(value) if value.to_string().len() > MAX_OUTPUT_BYTES => refuse(
                ToolErrorKind::ReplyTooLarge,
                format!(
                    "output of `{}` exceeds {MAX_OUTPUT_BYTES} bytes",
                    request.tool
                ),
            ),
            outcome => outcome,
        }
    }

    /// Handle one wire line and return the reply line. A line over
    /// [`MAX_REQUEST_BYTES`] is rejected before it is parsed, and a line that is
    /// not a valid request is answered with `bad_request`; neither is recorded
    /// as a call.
    /// ```
    /// use harness::harness_adapter::ArmedBroker;
    ///
    /// async fn line(broker: &ArmedBroker, text: &str) -> String {
    ///     broker.handle_line(text).await
    /// }
    /// ```
    pub async fn handle_line(&self, line: &str) -> String {
        let bad = |message: String| BrokerResponse {
            ok: false,
            output: json!(message),
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
        serde_json::to_string(&response).expect("broker responses are plain JSON")
    }
}

fn map_result(result: Result<Value, ToolError>) -> BrokerOutcome {
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
    };
    BrokerOutcome::ToolError {
        kind,
        message: error.to_string(),
    }
}
