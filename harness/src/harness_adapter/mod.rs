//! Harness-neutral seam between nanna's agent definitions and the agent
//! runtimes that execute them. Nothing here runs a runtime yet: the broker,
//! executor and `TaskRunner` switch are not written, so this module is
//! planned infrastructure, not a delivered isolation guarantee.
//!
//! An audited spawn ([`Allowed`](crate::auditor::Allowed)) is resolved against
//! the capabilities of its scoped registry into a [`ResolvedAgent`]. A
//! [`HarnessAdapter`] translates that into a draft [`LaunchPlan`]: files, argv,
//! environment, the one broker socket and the one model gateway the runtime
//! may reach. [`launch_plan`] checks the draft against [`IsolationPolicy`] and
//! returns a [`CheckedPlan`], the only thing an executor may accept.
//!
//! The runtime is meant to hold no tools of its own. Each capability it
//! exposes is a stub that forwards the call over the broker socket to nanna,
//! which runs the real [`Tool`](crate::tools::Tool) under the identity's scope,
//! effect ceiling and incident holds.
//!
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use harness::auditor::{AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary};
//! use harness::effects::EffectClass;
//! use harness::harness_adapter::{launch_plan, Endpoint, PiAdapter, ResolvedAgent, ScopedCapabilities};
//! use harness::identity::{DevLoop, IdentityCatalog};
//! use harness::tools::create_tool_registry;
//!
//! let dir = tempfile::tempdir().unwrap();
//! let card = |name: &str, max_effect: &str, tools: &str| format!(
//!     "[identity]\nname = \"{name}\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\n\
//!      system_prompt = {{ inline = \"p\" }}\n\n[scope]\nrepos = []\npaths = [\"**\"]\n\
//!      max_effect = \"{max_effect}\"\ntools = [{tools}]\n\n[limits]\nmax_iterations = 1\n\
//!      max_wall_clock_secs = 1\nmax_concurrent = 1\n"
//! );
//! std::fs::write(dir.path().join("reader.toml"), card("reader", "workspace", "\"read_file\", \"search\"")).unwrap();
//! std::fs::write(dir.path().join("auditor.toml"), card("auditor", "none", "")).unwrap();
//! let catalog = IdentityCatalog::load(dir.path()).unwrap();
//! let context = AuditContext::new(catalog.clone(), catalog.get("auditor").unwrap().clone()).unwrap();
//! let request = SpawnRequest {
//!     parent_task: TaskSummary::new("t1", "Read code", "github.com/example/repo"),
//!     identity: "reader".to_string(),
//!     subtask: "Summarise src/lib.rs.".to_string(),
//!     dev_loop: DevLoop::Inner,
//!     requested_effect: EffectClass::None,
//! };
//! let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
//! let allowed = gate.check(request, &context).await.unwrap();
//!
//! let base = create_tool_registry(std::path::Path::new("."));
//! let scoped = ScopedCapabilities::scope(base, allowed.identity());
//! let agent = ResolvedAgent::resolve(
//!     &allowed,
//!     &scoped,
//!     "Summarise src/lib.rs",
//!     Endpoint::new("model-gateway", 11434).unwrap(),
//!     "nanna-agent:pi",
//!     "/run/nanna/broker.sock".into(),
//! )
//! .unwrap();
//! assert_eq!(agent.capability_names(), vec!["read_file", "search"]);
//!
//! let plan = launch_plan(&PiAdapter, &agent).unwrap();
//! assert_eq!(plan.exposed_capabilities, vec!["read_file", "search"]);
//! # }
//! ```

mod checked;
mod isolation;
mod pi;
mod pi_extension;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use checked::{launch_plan, CheckedPlan, HarnessAdapter, PlanError};
pub use isolation::{IsolationPolicy, IsolationViolation, BROKER_SOCKET_CONTAINER_PATH};
pub use pi::PiAdapter;

use crate::auditor::Allowed;
use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityError, LimitsSection, ScopeSection};
use crate::tools::{ToolRegistry, PATH_UNSCOPABLE_TOOLS};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use thiserror::Error;

/// A host and port a runtime may reach over the network.
///
/// `Endpoint` deliberately does not implement `Deserialize`: every value goes
/// through [`Endpoint::new`] or [`Endpoint::operator_configured`].
///
/// ```compile_fail
/// use harness::harness_adapter::Endpoint;
///
/// let _: Endpoint = serde_json::from_str(r#"{"host":"gw:80/@evil.com#","port":80}"#).unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl Endpoint {
    /// Validate and build an endpoint. Hosts are restricted to ASCII letters,
    /// digits, `.` and `-` with well-formed labels. `localhost`, loopback,
    /// link-local, unspecified and multicast addresses are refused, and a host
    /// whose last label looks numeric (decimal or `0x` hex) must be a plain
    /// dotted-quad address, so resolver tricks like `2130706433` or `0x7f.1`
    /// cannot name a loopback address.
    ///
    /// ```
    /// use harness::harness_adapter::Endpoint;
    ///
    /// assert!(Endpoint::new("model-gateway", 11434).is_ok());
    /// assert!(Endpoint::new("10.0.0.5", 11434).is_ok());
    /// assert!(Endpoint::new("evil/../host", 80).is_err());
    /// assert!(Endpoint::new("localhost", 80).is_err());
    /// assert!(Endpoint::new("127.0.0.1", 80).is_err());
    /// assert!(Endpoint::new("2130706433", 80).is_err());
    /// ```
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, ResolveError> {
        let host = host.into();
        if port == 0 || !host_is_well_formed(&host) || host_is_ambient(&host) {
            return Err(ResolveError::InvalidEndpoint { host, port });
        }
        Ok(Self { host, port })
    }

    /// Build an endpoint the operator has configured explicitly, such as a
    /// gateway on `localhost`. Only the character set and port are checked.
    ///
    /// ```
    /// use harness::harness_adapter::Endpoint;
    ///
    /// assert!(Endpoint::operator_configured("localhost", 11434).is_ok());
    /// assert!(Endpoint::operator_configured("a b", 11434).is_err());
    /// ```
    pub fn operator_configured(host: impl Into<String>, port: u16) -> Result<Self, ResolveError> {
        let host = host.into();
        if port == 0 || !host_is_well_formed(&host) {
            return Err(ResolveError::InvalidEndpoint { host, port });
        }
        Ok(Self { host, port })
    }

    /// The host name or address.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

fn host_is_well_formed(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    for label in host.split('.') {
        let bad_edge = label.starts_with('-') || label.ends_with('-');
        if label.is_empty() || bad_edge {
            return false;
        }
        for c in label.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return false;
            }
        }
    }
    true
}

fn looks_numeric(label: &str) -> bool {
    let decimal = label.chars().all(|c| c.is_ascii_digit());
    let hex = label.len() > 2
        && (label.starts_with("0x") || label.starts_with("0X"))
        && label[2..].chars().all(|c| c.is_ascii_hexdigit());
    decimal || hex
}

fn host_is_ambient(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let last = host.rsplit('.').next().unwrap_or(host);
    if !looks_numeric(last) {
        return false;
    }
    match host.parse::<Ipv4Addr>() {
        Ok(ip) => {
            ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast()
        }
        Err(_) => true,
    }
}

/// One capability an agent may call: a tool nanna runs on its behalf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySpec {
    /// Tool name, as in identity `scope.tools` patterns.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema of the call arguments.
    pub parameters: serde_json::Value,
    /// Largest blast radius a call can reach.
    pub effect: EffectClass,
}

impl CapabilitySpec {
    /// Build a capability.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::harness_adapter::CapabilitySpec;
    ///
    /// let spec = CapabilitySpec::new("search", "Search files.", serde_json::json!({}), EffectClass::None);
    /// assert_eq!(spec.name, "search");
    /// ```
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        effect: EffectClass,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            effect,
        }
    }
}

/// The capabilities of a registry scoped to one identity, bound to that
/// identity's whole scope. It is built in one step from a base registry and the
/// identity, so a registry scoped by one identity cannot be paired with
/// another. Repo-local identities share their global identity's name while
/// narrowing it, so a name alone cannot tell them apart; the bound scope can.
///
/// The base registry should already carry the identity's path scope (for
/// example from `create_tool_registry_for`): `scoped_for` filters tool names
/// but does not attach path restrictions to file tools.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedCapabilities {
    identity: String,
    scope: ScopeSection,
    specs: Vec<CapabilitySpec>,
}

impl ScopedCapabilities {
    /// Scope `base` to `identity` with [`ToolRegistry::scoped_for`] and
    /// describe what remains, sorted by name.
    pub fn scope(base: ToolRegistry, identity: &AgentIdentity) -> Self {
        let registry = base.scoped_for(identity);
        let mut specs: Vec<CapabilitySpec> = registry
            .get_definitions()
            .into_iter()
            .filter_map(|def| {
                let effect = registry.effect_class_of(&def.function.name)?;
                Some(CapabilitySpec {
                    parameters: serde_json::to_value(&def.function.parameters)
                        .unwrap_or(serde_json::Value::Null),
                    name: def.function.name,
                    description: def.function.description,
                    effect,
                })
            })
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            identity: identity.name().to_string(),
            scope: identity.scope.clone(),
            specs,
        }
    }

    /// Name of the identity the capabilities were scoped by.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The scoped capabilities, sorted by name.
    pub fn specs(&self) -> &[CapabilitySpec] {
        &self.specs
    }
}

/// Failures resolving an audited spawn into a [`ResolvedAgent`].
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The capabilities were scoped by an identity whose name or scope differs
    /// from the one being resolved.
    #[error("capabilities were scoped by a different identity than `{identity}`")]
    ScopeMismatch {
        /// Identity being resolved.
        identity: String,
    },
    /// A granted capability exceeds what the identity allows. Never expected
    /// when the grant is built with [`ScopedCapabilities::scope`]; it exists
    /// so a widened grant fails closed.
    #[error("capability `{tool}` is wider than identity `{identity}` allows")]
    GrantWiderThanIdentity {
        /// Identity being resolved.
        identity: String,
        /// Offending capability.
        tool: String,
    },
    /// The identity's system prompt could not be read.
    #[error("system prompt: {0}")]
    Prompt(#[from] IdentityError),
    /// The model endpoint is not an acceptable host and non-zero port.
    #[error("invalid model endpoint {host}:{port}")]
    InvalidEndpoint {
        /// Offending host.
        host: String,
        /// Offending port.
        port: u16,
    },
    /// The model name could be read as a command-line option or contains
    /// whitespace or control characters.
    #[error("invalid model name `{model}`")]
    InvalidModel {
        /// Offending model name.
        model: String,
    },
    /// The broker socket path is empty, relative, `/`, escapes with `..`, or
    /// names a container runtime's control socket.
    #[error("invalid broker socket path `{path}`")]
    InvalidBrokerSocket {
        /// Offending path.
        path: String,
    },
}

fn model_is_valid(model: &str) -> bool {
    let plain = !model.is_empty() && !model.starts_with('-');
    plain && model.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

fn broker_socket_is_valid(path: &str) -> bool {
    let escapes = path.split(['/', '\\']).any(|part| part == "..");
    let runtime_socket = ["docker.sock", "podman.sock", "containerd.sock"]
        .iter()
        .any(|name| path.ends_with(name));
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains('\0')
        && !escapes
        && !runtime_socket
}

/// What an identity may touch, carried so that whatever enforces it cannot
/// be handed a plan that forgot it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeGrant {
    /// Repositories the agent may be spawned against.
    pub repos: Vec<String>,
    /// Writable globs relative to the worktree root.
    pub paths: Vec<String>,
    /// Readable globs; `None` leaves reads unrestricted inside the worktree.
    pub read_paths: Option<Vec<String>>,
}

/// An audited spawn bound to a task: everything a runtime needs, already
/// narrowed to what the identity's scope permits. Fields are private and can
/// only come from [`ResolvedAgent::resolve`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgent {
    name: String,
    system_prompt: String,
    task_prompt: String,
    model: String,
    endpoint: Endpoint,
    max_effect: EffectClass,
    limits: LimitsSection,
    scope: ScopeGrant,
    image: String,
    broker_socket: PathBuf,
    capabilities: Vec<CapabilitySpec>,
}

impl ResolvedAgent {
    /// Resolve an audited spawn against the capabilities of the registry
    /// scoped to its identity. The identity is taken from `allowed`, never from
    /// the caller, so an agent can only be resolved from the identity the
    /// auditor saw. The grant is exactly the scoped set: there is no second
    /// filter, only an assertion that nothing in it exceeds the identity's
    /// tool patterns, effect ceiling, or (for a path-restricted identity) the
    /// tools that cannot honour path scope.
    pub fn resolve(
        allowed: &Allowed,
        scoped: &ScopedCapabilities,
        task_prompt: impl Into<String>,
        endpoint: Endpoint,
        image: impl Into<String>,
        broker_socket: PathBuf,
    ) -> Result<Self, ResolveError> {
        let identity = allowed.identity();
        if scoped.identity != identity.name() || scoped.scope != identity.scope {
            return Err(ResolveError::ScopeMismatch {
                identity: identity.name().to_string(),
            });
        }
        let restricted = identity.restricts_paths();
        for spec in &scoped.specs {
            let unscopable = restricted && PATH_UNSCOPABLE_TOOLS.contains(&spec.name.as_str());
            if unscopable
                || !identity.allows_tool(&spec.name)
                || !identity.allows_effect(spec.effect)
            {
                return Err(ResolveError::GrantWiderThanIdentity {
                    identity: identity.name().to_string(),
                    tool: spec.name.clone(),
                });
            }
        }
        let model = identity.identity.model.clone();
        if !model_is_valid(&model) {
            return Err(ResolveError::InvalidModel { model });
        }
        let socket_text = broker_socket.to_string_lossy().into_owned();
        if !broker_socket_is_valid(&socket_text) {
            return Err(ResolveError::InvalidBrokerSocket { path: socket_text });
        }
        Ok(Self {
            name: identity.name().to_string(),
            system_prompt: identity.system_prompt_text()?,
            task_prompt: task_prompt.into(),
            model,
            endpoint,
            max_effect: identity.scope.max_effect,
            limits: identity.limits,
            scope: ScopeGrant {
                repos: identity.scope.repos.clone(),
                paths: identity.scope.paths.clone(),
                read_paths: identity.scope.read_paths.clone(),
            },
            image: image.into(),
            broker_socket,
            capabilities: scoped.specs.clone(),
        })
    }

    /// Drop every capability whose effect class exceeds `max`, so the runtime
    /// is never shown a tool the broker would refuse.
    pub fn withhold_above(mut self, max: EffectClass) -> Self {
        self.capabilities.retain(|c| c.effect <= max);
        self
    }

    /// Identity name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// System prompt text.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// The task the agent is asked to perform.
    pub fn task_prompt(&self) -> &str {
        &self.task_prompt
    }

    /// Model identifier at the gateway.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The only network endpoint the runtime may reach.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Highest effect class any capability may reach.
    pub fn max_effect(&self) -> EffectClass {
        self.max_effect
    }

    /// Resource ceilings for the run.
    pub fn limits(&self) -> LimitsSection {
        self.limits
    }

    /// Path and repository scope the broker must enforce.
    pub fn scope(&self) -> &ScopeGrant {
        &self.scope
    }

    /// Container image the runtime starts from.
    pub fn image(&self) -> &str {
        &self.image
    }

    /// Host path of the broker socket. The executor must also confirm it is a
    /// socket it created and owns before mounting it.
    pub fn broker_socket(&self) -> &std::path::Path {
        &self.broker_socket
    }

    /// The capabilities granted, sorted by name.
    pub fn capabilities(&self) -> &[CapabilitySpec] {
        &self.capabilities
    }

    /// Names of the granted capabilities, sorted.
    pub fn capability_names(&self) -> Vec<&str> {
        self.capabilities.iter().map(|c| c.name.as_str()).collect()
    }
}

/// A file written into the container before the runtime starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    /// Absolute path inside the container, under `/nanna/`.
    pub path: PathBuf,
    /// File contents.
    pub contents: String,
}

/// Everything an executor needs to start a runtime, and nothing it can use to
/// widen the sandbox: privileges, Linux capabilities, devices and any mount
/// other than the broker socket are not expressible here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Container image.
    pub image: String,
    /// Files to place in the container.
    pub files: Vec<PlanFile>,
    /// Command and arguments.
    pub argv: Vec<String>,
    /// Environment variables.
    pub env: Vec<(String, String)>,
    /// Host path of the broker socket, mounted at
    /// [`BROKER_SOCKET_CONTAINER_PATH`]. It is the only mount.
    pub broker_socket: PathBuf,
    /// The only network endpoint: the model gateway.
    pub gateway: Endpoint,
    /// Lines written to the runtime's stdin once it starts. Only the executor
    /// may write to a runtime's RPC stdin.
    pub stdin: Vec<String>,
    /// Names of the capabilities the runtime will expose to the model.
    pub exposed_capabilities: Vec<String>,
    /// Ceilings the executor enforces: the runtime has no iteration cap of
    /// its own, so the executor counts turns and aborts on the deadline.
    pub limits: LimitsSection,
    /// Scope the broker enforces on every capability call.
    pub scope: ScopeGrant,
}

/// The runtime reported a tool set that differs from the granted one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("runtime tools differ from the plan: unexpected {unexpected:?}, missing {missing:?}")]
pub struct HandshakeMismatch {
    /// Tools the runtime has that the plan did not grant.
    pub unexpected: Vec<String>,
    /// Granted tools the runtime does not have.
    pub missing: Vec<String>,
}

/// Compare the tools a started runtime reports against the plan. Executors
/// must call this after start and abort the run on any mismatch, because a
/// runtime that ignores a lockdown flag fails open. For pi 1.2.0 the tool list
/// appears only in the first system `message_start` event (`toolsAdded`),
/// emitted after a prompt is sent, so the broker must refuse every call until
/// this check has passed.
pub fn verify_runtime_tools(
    plan: &CheckedPlan,
    reported: &[String],
) -> Result<(), HandshakeMismatch> {
    let granted: std::collections::BTreeSet<&String> = plan.exposed_capabilities.iter().collect();
    let seen: std::collections::BTreeSet<&String> = reported.iter().collect();
    let unexpected: Vec<String> = seen.difference(&granted).map(|s| (*s).clone()).collect();
    let missing: Vec<String> = granted.difference(&seen).map(|s| (*s).clone()).collect();
    if unexpected.is_empty() && missing.is_empty() {
        Ok(())
    } else {
        Err(HandshakeMismatch {
            unexpected,
            missing,
        })
    }
}

/// An agent definition a runtime cannot honour. Adapters fail closed with
/// this instead of dropping a constraint.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Unsupported {
    /// The agent has no capabilities, so there is nothing for it to do.
    #[error("agent `{agent}` has no capabilities")]
    NoCapabilities {
        /// Agent name.
        agent: String,
    },
    /// A capability name cannot be represented safely by the runtime.
    #[error("capability name `{name}` is not representable: {reason}")]
    CapabilityName {
        /// Offending name.
        name: String,
        /// Why it is rejected.
        reason: String,
    },
}
