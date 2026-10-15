//! Host-side capability broker: the single door through which a sandboxed
//! runtime acts.
//!
//! The broker owns the identity's scoped [`ToolRegistry`] and calls
//! [`ToolRegistry::execute`] for every request. It reimplements no gate: scope,
//! path restrictions, protected paths and the production hold stay where they
//! are, inside the registry and its tools. It only decides which requests reach
//! the registry, maps the registry's errors onto what the runtime and the
//! executor must do, and keeps the audit log that becomes the task result's
//! tool-call history.
//!
//! A broker serves nothing until the executor has armed it with
//! [`Broker::arm`], which checks the launch plan against this broker and the
//! tool set the started runtime reports. After a halt it serves nothing more.

use super::{verify_runtime_tools, CheckedPlan, HandshakeMismatch, ResolvedAgent};
use crate::auditor::Allowed;
use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, LimitsSection};
use crate::scope::ScopeError;
use crate::tools::{ToolError, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use thiserror::Error;

/// Largest tool output the broker will return, in bytes of its JSON text. It is
/// below the extension's reply cap so the reply envelope still fits.
pub const MAX_OUTPUT_BYTES: usize = 1_000_000;

/// Which effect classes the broker will run. The default is
/// [`EffectClass::Workspace`]: the per-action gate (window, lease, ceiling,
/// escalation) is not on main yet, so anything above Workspace would run with
/// no window or lease check. Raise it only when that gate is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerPolicy {
    max_effect: EffectClass,
}

impl BrokerPolicy {
    /// A policy admitting every class up to and including `max_effect`.
    pub const fn new(max_effect: EffectClass) -> Self {
        Self { max_effect }
    }

    /// Whether a capability of class `effect` may run.
    pub fn admits(&self, effect: EffectClass) -> bool {
        effect <= self.max_effect
    }

    /// The highest admitted class.
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
#[derive(Debug, Error)]
pub enum BrokerError {
    /// The registry builder rejected the identity's scope.
    #[error("scope: {0}")]
    Scope(#[from] ScopeError),
    /// The audited identity is not the one the agent was resolved from.
    #[error("audited identity `{identity}` does not match the resolved agent `{agent}`")]
    IdentityMismatch {
        /// Identity from the audit proof.
        identity: String,
        /// Agent the broker was built for.
        agent: String,
    },
    /// The registry the builder returned is not scoped to the identity.
    #[error("registry is not scoped to identity `{identity}`")]
    RegistryNotScoped {
        /// Identity the registry should have been scoped to.
        identity: String,
    },
}

/// Why a broker refused to be armed.
#[derive(Debug, Error)]
pub enum ArmError {
    /// The plan was built for a different agent, repository, scope, limits or
    /// broker socket than this broker.
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
pub struct BrokerRequest {
    /// Capability name.
    pub tool: String,
    /// Call arguments.
    #[serde(default)]
    pub args: Value,
}

/// The reply written back to the runtime, one JSON object per line.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    /// The broker has not been armed yet.
    NotReady,
    /// The task already halted; nothing more is served.
    TaskHalted,
    /// The capability was never granted to this agent.
    NotGranted,
    /// The request was not valid JSON of the expected shape.
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

/// Why the task must stop.
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
    /// The wire reply for this outcome.
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
            BrokerOutcome::Halt(HaltReason::ProductionHeld {
                repo,
                escalation_id,
                summary,
            }) => BrokerResponse {
                ok: false,
                output: json!(format!(
                    "production effects held for {repo} by incident {escalation_id}: {summary}"
                )),
                kind: None,
                halt: true,
            },
        }
    }
}

/// How a recorded call ended.
#[derive(Debug, Clone, PartialEq)]
pub enum AuditOutcome {
    /// The tool ran successfully.
    Ok,
    /// The call returned an error to the agent.
    Error(ToolErrorKind),
    /// The call stopped the task.
    Halt,
}

/// One broker decision, in call order. This log is the authoritative source
/// for the task result's tool calls and denials.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    /// Capability name as requested.
    pub tool: String,
    /// Call arguments as requested.
    pub args: Value,
    /// Effect class when the capability is one the agent was granted.
    pub effect: Option<EffectClass>,
    /// How the call ended.
    pub outcome: AuditOutcome,
}

/// The broker for one task.
pub struct Broker {
    agent: String,
    audited_repo: String,
    limits: LimitsSection,
    broker_socket: std::path::PathBuf,
    registry: ToolRegistry,
    served: BTreeSet<String>,
    effects: Vec<(String, EffectClass)>,
    audit: Mutex<Vec<AuditEntry>>,
    armed: AtomicBool,
    halted: AtomicBool,
}

impl Broker {
    /// Build the broker for `agent`. The identity comes from the audit proof
    /// `allowed`, never from the caller. `build` receives that identity and must
    /// return the registry scoped to it with path restrictions attached (for
    /// example `create_container_tool_registry_for`); no externally built
    /// registry is accepted.
    ///
    /// `agent` must have been resolved from the same audited spawn: same
    /// identity name, scope and ceiling, and the same audited repository.
    /// Capabilities above `policy` are not served; a plan that exposes one cannot
    /// arm the broker.
    pub fn for_agent(
        allowed: &Allowed,
        agent: &ResolvedAgent,
        policy: BrokerPolicy,
        build: impl FnOnce(&AgentIdentity) -> Result<ToolRegistry, ScopeError>,
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
        let registry = build(identity)?;
        if registry.identity() != Some(identity.name()) {
            return Err(BrokerError::RegistryNotScoped {
                identity: identity.name().to_string(),
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
            agent: agent.name().to_string(),
            audited_repo: agent.audited_repo().to_string(),
            limits: agent.limits(),
            broker_socket: agent.broker_socket().to_path_buf(),
            registry,
            served,
            effects,
            audit: Mutex::new(Vec::new()),
            armed: AtomicBool::new(false),
            halted: AtomicBool::new(false),
        })
    }

    /// Names the broker will run, sorted.
    pub fn served(&self) -> Vec<&str> {
        self.served.iter().map(String::as_str).collect()
    }

    /// Whether the broker has been armed.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    /// Whether the task has halted.
    pub fn is_halted(&self) -> bool {
        self.halted.load(Ordering::SeqCst)
    }

    /// Arm the broker. The executor calls this once the runtime has started and
    /// reported its tools (for pi, the `toolsAdded` list in the first system
    /// `message_start`). It checks that `plan` was built for this broker, that
    /// every exposed capability is one the broker serves, and that the runtime's
    /// reported tools equal the plan's. Until it succeeds, every call is
    /// refused, because the model already holds its tools before the check.
    pub fn arm(&self, plan: &CheckedPlan, reported: &[String]) -> Result<(), ArmError> {
        let checks: [(&'static str, bool); 4] = [
            ("agent", plan.agent == self.agent),
            ("audited repository", plan.audited_repo == self.audited_repo),
            ("limits", plan.limits == self.limits),
            ("broker socket", plan.broker_socket == self.broker_socket),
        ];
        if let Some((field, _)) = checks.iter().find(|(_, same)| !same) {
            return Err(ArmError::PlanMismatch { field });
        }
        let unserved: Vec<String> = plan
            .exposed_capabilities
            .iter()
            .filter(|name| !self.served.contains(name.as_str()))
            .cloned()
            .collect();
        if !unserved.is_empty() {
            return Err(ArmError::ExposedNotServed { names: unserved });
        }
        verify_runtime_tools(plan, reported)?;
        self.armed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Every decision so far, in order.
    pub fn audit(&self) -> Vec<AuditEntry> {
        self.audit.lock().expect("audit log poisoned").clone()
    }

    /// Handle one request.
    pub async fn handle(&self, request: BrokerRequest) -> BrokerOutcome {
        let effect = self
            .effects
            .iter()
            .find(|(name, _)| *name == request.tool)
            .map(|(_, effect)| *effect);
        let outcome = self.decide(&request).await;
        if matches!(outcome, BrokerOutcome::Halt(_)) {
            self.halted.store(true, Ordering::SeqCst);
        }
        let audited = match &outcome {
            BrokerOutcome::Output(_) => AuditOutcome::Ok,
            BrokerOutcome::ToolError { kind, .. } => AuditOutcome::Error(*kind),
            BrokerOutcome::Halt(_) => AuditOutcome::Halt,
        };
        self.audit
            .lock()
            .expect("audit log poisoned")
            .push(AuditEntry {
                tool: request.tool,
                args: request.args,
                effect,
                outcome: audited,
            });
        outcome
    }

    async fn decide(&self, request: &BrokerRequest) -> BrokerOutcome {
        let refuse = |kind, message: String| BrokerOutcome::ToolError { kind, message };
        if self.is_halted() {
            return refuse(ToolErrorKind::TaskHalted, "the task has halted".into());
        }
        if !self.is_armed() {
            return refuse(ToolErrorKind::NotReady, "the broker is not armed".into());
        }
        if !self.served.contains(&request.tool) {
            return refuse(
                ToolErrorKind::NotGranted,
                format!("capability `{}` is not granted", request.tool),
            );
        }
        let result = self
            .registry
            .execute(&request.tool, request.args.clone())
            .await;
        match map_result(result) {
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

    /// Handle one wire line and return the reply line. A line that is not a
    /// valid request is answered with a `bad_request` error and is not logged
    /// as a call.
    pub async fn handle_line(&self, line: &str) -> String {
        let response = match serde_json::from_str::<BrokerRequest>(line) {
            Ok(request) => self.handle(request).await.to_response(),
            Err(error) => BrokerResponse {
                ok: false,
                output: json!(format!("bad request: {error}")),
                kind: Some(ToolErrorKind::BadRequest),
                halt: false,
            },
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
