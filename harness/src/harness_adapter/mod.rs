//! Harness-neutral seam between nanna's agent definitions and the agent
//! runtimes that execute them.
//!
//! An [`AgentIdentity`](crate::identity::AgentIdentity) is resolved against
//! the capabilities a task can offer into a [`ResolvedAgent`]. A
//! [`HarnessAdapter`] translates that into a [`LaunchPlan`]: files, argv,
//! environment and the only mounts and network endpoints the runtime may
//! have. Every plan passes through [`IsolationPolicy`] before an executor
//! sees it, so a new adapter cannot widen what an agent can reach.
//!
//! The runtime holds no tools of its own. Each capability it exposes is a
//! stub that forwards the call over the broker socket to nanna, which runs
//! the real [`Tool`](crate::tools::Tool) under the identity's scope, effect
//! ceiling and incident holds.
//!
//! ```
//! use harness::effects::EffectClass;
//! use harness::harness_adapter::{
//!     launch_plan, CapabilitySpec, Endpoint, HarnessKind, PiAdapter, ResolvedAgent,
//! };
//! use harness::identity::AgentIdentity;
//!
//! let identity = AgentIdentity::from_toml_str(
//!     r#"
//! [identity]
//! name = "reader"
//! description = "Reads code."
//! loop = "inner"
//! model = "gemma4:e4b"
//! system_prompt = { inline = "Read carefully." }
//!
//! [scope]
//! repos = ["github.com/example/repo"]
//! paths = ["src/**"]
//! max_effect = "workspace"
//! tools = ["read_file", "search"]
//!
//! [limits]
//! max_iterations = 10
//! max_wall_clock_secs = 60
//! max_concurrent = 1
//! "#,
//!     "reader.toml",
//! )
//! .unwrap();
//!
//! let available = vec![
//!     CapabilitySpec::new("read_file", "Read a file.", serde_json::json!({"type": "object"}), EffectClass::None),
//!     CapabilitySpec::new("run_command", "Run a command.", serde_json::json!({"type": "object"}), EffectClass::Workspace),
//! ];
//! let agent = ResolvedAgent::resolve(
//!     &identity,
//!     &available,
//!     "Summarise src/lib.rs",
//!     Endpoint::new("model-gateway", 11434).unwrap(),
//!     "nanna-agent:pi",
//!     "/run/nanna/broker.sock".into(),
//! )
//! .unwrap();
//! assert_eq!(agent.capability_names(), vec!["read_file"]);
//!
//! let plan = launch_plan(&PiAdapter::default(), &agent).unwrap();
//! assert_eq!(plan.exposed_capabilities, vec!["read_file"]);
//! assert_eq!(HarnessKind::Pi.as_str(), "pi");
//! ```

mod isolation;
mod pi;

pub use isolation::{IsolationPolicy, IsolationViolation, BROKER_SOCKET_CONTAINER_PATH};
pub use pi::PiAdapter;

use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityError, LimitsSection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// Agent runtimes nanna can launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HarnessKind {
    /// The pi coding agent, driven over its RPC mode.
    Pi,
}

impl HarnessKind {
    /// Stable lowercase name, identical to the serde representation.
    ///
    /// ```
    /// use harness::harness_adapter::HarnessKind;
    ///
    /// assert_eq!(HarnessKind::Pi.as_str(), "pi");
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
            HarnessKind::Pi => "pi",
        }
    }
}

/// A host and port a runtime may reach over the network.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl Endpoint {
    /// Validate and build an endpoint. Hosts are restricted to ASCII letters,
    /// digits, `.` and `-` so they can be embedded in generated URLs.
    ///
    /// ```
    /// use harness::harness_adapter::Endpoint;
    ///
    /// assert!(Endpoint::new("model-gateway", 11434).is_ok());
    /// assert!(Endpoint::new("evil/../host", 80).is_err());
    /// assert!(Endpoint::new("", 80).is_err());
    /// ```
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, ResolveError> {
        let host = host.into();
        let valid = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
        if !valid || port == 0 {
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

/// Failures resolving an identity into a [`ResolvedAgent`].
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The identity's system prompt could not be read.
    #[error("system prompt: {0}")]
    Prompt(#[from] IdentityError),
    /// The model endpoint is not a plain host and non-zero port.
    #[error("invalid model endpoint {host}:{port}")]
    InvalidEndpoint {
        /// Offending host.
        host: String,
        /// Offending port.
        port: u16,
    },
}

/// An identity bound to a task: everything a runtime needs, already narrowed
/// to what the identity's scope permits.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgent {
    /// Identity name.
    pub name: String,
    /// System prompt text.
    pub system_prompt: String,
    /// The task the agent is asked to perform.
    pub task_prompt: String,
    /// Model identifier at the gateway.
    pub model: String,
    /// The only network endpoint the runtime may reach.
    pub endpoint: Endpoint,
    /// Highest effect class any capability may reach.
    pub max_effect: EffectClass,
    /// Resource ceilings for the run.
    pub limits: LimitsSection,
    /// Container image the runtime starts from.
    pub image: String,
    /// Host path of the broker socket mounted into the container.
    pub broker_socket: PathBuf,
    capabilities: Vec<CapabilitySpec>,
}

impl ResolvedAgent {
    /// Resolve `identity` against `available`, keeping only the capabilities
    /// the identity's tool patterns allow and whose effect class is within its
    /// ceiling, sorted by name.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::harness_adapter::{CapabilitySpec, Endpoint, ResolvedAgent};
    /// use harness::identity::AgentIdentity;
    ///
    /// let toml = r#"
    /// [identity]
    /// name = "a"
    /// description = "d"
    /// loop = "inner"
    /// model = "m"
    /// system_prompt = { inline = "p" }
    /// [scope]
    /// repos = ["r"]
    /// paths = ["**"]
    /// max_effect = "workspace"
    /// tools = ["*"]
    /// [limits]
    /// max_iterations = 1
    /// max_wall_clock_secs = 1
    /// max_concurrent = 1
    /// "#;
    /// let identity = AgentIdentity::from_toml_str(toml, "a.toml").unwrap();
    /// let available = vec![
    ///     CapabilitySpec::new("deploy", "d", serde_json::json!({}), EffectClass::Production),
    ///     CapabilitySpec::new("read_file", "r", serde_json::json!({}), EffectClass::None),
    /// ];
    /// let agent = ResolvedAgent::resolve(
    ///     &identity, &available, "t", Endpoint::new("gw", 1).unwrap(), "img", "/s".into(),
    /// ).unwrap();
    /// assert_eq!(agent.capability_names(), vec!["read_file"]);
    /// ```
    pub fn resolve(
        identity: &AgentIdentity,
        available: &[CapabilitySpec],
        task_prompt: impl Into<String>,
        endpoint: Endpoint,
        image: impl Into<String>,
        broker_socket: PathBuf,
    ) -> Result<Self, ResolveError> {
        let mut capabilities: Vec<CapabilitySpec> = available
            .iter()
            .filter(|spec| identity.allows_tool(&spec.name) && identity.allows_effect(spec.effect))
            .cloned()
            .collect();
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        capabilities.dedup_by(|a, b| a.name == b.name);
        Ok(Self {
            name: identity.name().to_string(),
            system_prompt: identity.system_prompt_text()?,
            task_prompt: task_prompt.into(),
            model: identity.identity.model.clone(),
            endpoint,
            max_effect: identity.scope.max_effect,
            limits: identity.limits,
            image: image.into(),
            broker_socket,
            capabilities,
        })
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
    /// Absolute path inside the container.
    pub path: PathBuf,
    /// File contents.
    pub contents: String,
}

/// A host path mounted into the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Path on the host.
    pub host: PathBuf,
    /// Path inside the container.
    pub container: PathBuf,
    /// Whether the mount is read-only.
    pub read_only: bool,
}

/// Network access granted to the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// No network at all.
    None,
    /// Only these endpoints.
    Only(Vec<Endpoint>),
}

/// Everything an executor needs to start a runtime, and nothing it can use to
/// widen the sandbox: privileges, capabilities and device access are fixed by
/// the executor, not expressible here.
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
    /// Host mounts.
    pub mounts: Vec<Mount>,
    /// Network access.
    pub network: NetworkPolicy,
    /// Lines written to the runtime's stdin once it starts.
    pub stdin: Vec<String>,
    /// Names of the capabilities the runtime will expose to the model.
    pub exposed_capabilities: Vec<String>,
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
    /// The model identifier is empty.
    #[error("agent `{agent}` names no model")]
    NoModel {
        /// Agent name.
        agent: String,
    },
}

/// Why a launch plan was refused.
#[derive(Debug, Error)]
pub enum PlanError {
    /// The adapter cannot honour the agent definition.
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    /// The adapter produced a plan that breaks the isolation policy.
    #[error(transparent)]
    Isolation(#[from] IsolationViolation),
}

/// Translates a [`ResolvedAgent`] into a [`LaunchPlan`] for one runtime.
///
/// Implementations are pure: no I/O, no clock, no environment.
pub trait HarnessAdapter {
    /// The runtime this adapter targets.
    fn kind(&self) -> HarnessKind;

    /// Build the launch plan, or refuse with [`Unsupported`].
    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported>;
}

/// Build a plan with `adapter` and verify it against [`IsolationPolicy`].
/// Executors must obtain plans only through this function.
///
/// ```
/// use harness::harness_adapter::{
///     launch_plan, CapabilitySpec, Endpoint, PiAdapter, ResolvedAgent,
/// };
/// use harness::identity::AgentIdentity;
///
/// let toml = r#"
/// [identity]
/// name = "a"
/// description = "d"
/// loop = "inner"
/// model = "m"
/// system_prompt = { inline = "p" }
/// [scope]
/// repos = ["r"]
/// paths = ["**"]
/// max_effect = "none"
/// tools = ["nothing_matches"]
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 1
/// max_concurrent = 1
/// "#;
/// let identity = AgentIdentity::from_toml_str(toml, "a.toml").unwrap();
/// let agent = ResolvedAgent::resolve(
///     &identity, &[], "t", Endpoint::new("gw", 1).unwrap(), "img", "/s".into(),
/// ).unwrap();
/// assert!(launch_plan(&PiAdapter::default(), &agent).is_err());
/// ```
pub fn launch_plan(
    adapter: &dyn HarnessAdapter,
    agent: &ResolvedAgent,
) -> Result<LaunchPlan, PlanError> {
    let plan = adapter.plan(agent)?;
    IsolationPolicy::check(&plan, agent)?;
    Ok(plan)
}
