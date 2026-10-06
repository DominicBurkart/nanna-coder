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
//! use harness::harness_adapter::{
//!     launch_plan, Endpoint, HarnessKind, PiAdapter, ResolvedAgent, ScopedCapabilities,
//! };
//! use harness::tools::create_tool_registry;
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
//! let scoped = ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
//! let agent = ResolvedAgent::resolve(
//!     &identity,
//!     &scoped,
//!     "Summarise src/lib.rs",
//!     Endpoint::new("model-gateway", 11434).unwrap(),
//!     "nanna-agent:pi",
//!     "/run/nanna/broker.sock".into(),
//! )
//! .unwrap();
//! assert_eq!(agent.capability_names(), vec!["read_file", "search"]);
//!
//! let plan = launch_plan(&PiAdapter::default(), &agent).unwrap();
//! assert_eq!(plan.exposed_capabilities, vec!["read_file", "search"]);
//! assert_eq!(HarnessKind::Pi.as_str(), "pi");
//! ```

mod isolation;
mod pi;
#[cfg(test)]
mod testing;

pub use isolation::{IsolationPolicy, IsolationViolation, BROKER_SOCKET_CONTAINER_PATH};
pub use pi::PiAdapter;

use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityError, LimitsSection, ScopeSection};
use crate::tools::ToolRegistry;
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

/// The capabilities of a registry scoped to one identity, bound to that
/// identity's whole scope. It is built in one step from a base registry and the
/// identity, so a registry scoped by one identity cannot be paired with
/// another. Repo-local identities share their global identity's name while
/// narrowing it, so a name alone cannot tell them apart; the bound scope can.
///
/// ```
/// use harness::harness_adapter::ScopedCapabilities;
/// use harness::identity::AgentIdentity;
/// use harness::tools::create_tool_registry;
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
/// tools = ["read_file"]
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 1
/// max_concurrent = 1
/// "#;
/// let identity = AgentIdentity::from_toml_str(toml, "a.toml").unwrap();
/// let scoped = ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
/// assert_eq!(scoped.identity(), "a");
/// assert_eq!(scoped.specs().len(), 1);
/// ```
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

/// Failures resolving an identity into a [`ResolvedAgent`].
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
    /// The model endpoint is not a plain host and non-zero port.
    #[error("invalid model endpoint {host}:{port}")]
    InvalidEndpoint {
        /// Offending host.
        host: String,
        /// Offending port.
        port: u16,
    },
}

/// What an identity may touch, carried so that whatever enforces it can
/// not be handed a plan that forgot it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeGrant {
    /// Repositories the agent may be spawned against.
    pub repos: Vec<String>,
    /// Writable globs relative to the worktree root.
    pub paths: Vec<String>,
    /// Readable globs; `None` leaves reads unrestricted inside the worktree.
    pub read_paths: Option<Vec<String>>,
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
    /// Path and repository scope the broker must enforce.
    pub scope: ScopeGrant,
    /// Container image the runtime starts from.
    pub image: String,
    /// Host path of the broker socket mounted into the container.
    pub broker_socket: PathBuf,
    capabilities: Vec<CapabilitySpec>,
}

impl ResolvedAgent {
    /// Resolve `identity` against the capabilities of the registry scoped to
    /// it. The grant is exactly that set: there is no second filter, so what
    /// the agent is shown is what the broker's registry enforces. Fails when
    /// the capabilities were scoped by an identity with a different name or
    /// scope, and refuses any capability the identity does not allow.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::harness_adapter::{Endpoint, ResolvedAgent, ScopedCapabilities};
    /// use harness::identity::AgentIdentity;
    /// use harness::tools::create_tool_registry;
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
    /// let scoped = ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
    /// let agent = ResolvedAgent::resolve(
    ///     &identity, &scoped, "t", Endpoint::new("gw", 1).unwrap(), "img", "/s".into(),
    /// ).unwrap();
    /// assert!(agent.capability_names().iter().all(|n| *n != "run_command"));
    /// assert!(agent.capabilities().iter().all(|c| c.effect <= EffectClass::Workspace));
    /// ```
    pub fn resolve(
        identity: &AgentIdentity,
        scoped: &ScopedCapabilities,
        task_prompt: impl Into<String>,
        endpoint: Endpoint,
        image: impl Into<String>,
        broker_socket: PathBuf,
    ) -> Result<Self, ResolveError> {
        if scoped.identity != identity.name() || scoped.scope != identity.scope {
            return Err(ResolveError::ScopeMismatch {
                identity: identity.name().to_string(),
            });
        }
        if let Some(spec) = scoped
            .specs
            .iter()
            .find(|s| !identity.allows_tool(&s.name) || !identity.allows_effect(s.effect))
        {
            return Err(ResolveError::GrantWiderThanIdentity {
                identity: identity.name().to_string(),
                tool: spec.name.clone(),
            });
        }
        Ok(Self {
            name: identity.name().to_string(),
            system_prompt: identity.system_prompt_text()?,
            task_prompt: task_prompt.into(),
            model: identity.identity.model.clone(),
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
            capabilities: scoped.specs().to_vec(),
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
pub enum PlanNetwork {
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
    pub network: PlanNetwork,
    /// Lines written to the runtime's stdin once it starts.
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
/// runtime that ignores a lockdown flag fails open.
///
/// ```
/// use harness::harness_adapter::{verify_runtime_tools, LaunchPlan, PlanNetwork, ScopeGrant};
/// use harness::identity::LimitsSection;
///
/// let plan = LaunchPlan {
///     image: "i".into(), files: vec![], argv: vec!["x".into()], env: vec![], mounts: vec![],
///     network: PlanNetwork::None, stdin: vec![],
///     exposed_capabilities: vec!["read_file".into()],
///     limits: LimitsSection { max_iterations: 1, max_wall_clock_secs: 1, max_concurrent: 1 },
///     scope: ScopeGrant { repos: vec![], paths: vec![], read_paths: None },
/// };
/// assert!(verify_runtime_tools(&plan, &["read_file".to_string()]).is_ok());
/// let err = verify_runtime_tools(&plan, &["read_file".to_string(), "bash".to_string()]).unwrap_err();
/// assert_eq!(err.unexpected, vec!["bash"]);
/// ```
pub fn verify_runtime_tools(
    plan: &LaunchPlan,
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
///     launch_plan, Endpoint, PiAdapter, ResolvedAgent, ScopedCapabilities,
/// };
/// use harness::identity::AgentIdentity;
/// use harness::tools::create_tool_registry;
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
/// let scoped = ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
/// let agent = ResolvedAgent::resolve(
///     &identity, &scoped, "t", Endpoint::new("gw", 1).unwrap(), "img", "/s".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;
    use crate::tools::create_tool_registry;

    fn identity_with(prompt: &str, tools: &str, effect: &str) -> AgentIdentity {
        let toml = format!(
            r#"
[identity]
name = "a"
description = "d"
loop = "middle"
model = "m"
system_prompt = {prompt}
[scope]
repos = ["github.com/x/y"]
paths = ["api/**"]
read_paths = ["**"]
max_effect = "{effect}"
tools = [{tools}]
[limits]
max_iterations = 7
max_wall_clock_secs = 9
max_concurrent = 2
"#
        );
        AgentIdentity::from_toml_str(&toml, "a.toml").unwrap()
    }

    fn inline() -> &'static str {
        "{ inline = \"p\" }"
    }

    fn resolve(identity: &AgentIdentity, available: &[CapabilitySpec]) -> ResolvedAgent {
        let scoped = testing::scoped(identity, available);
        ResolvedAgent::resolve(
            identity,
            &scoped,
            "t",
            Endpoint::new("gw", 1).unwrap(),
            "img",
            "/s".into(),
        )
        .unwrap()
    }

    #[test]
    fn endpoint_rejects_port_zero_and_exposes_parts() {
        assert!(Endpoint::new("gw", 0).is_err());
        let endpoint = Endpoint::new("gw.local", 8080).unwrap();
        assert_eq!((endpoint.host(), endpoint.port()), ("gw.local", 8080));
        let err = Endpoint::new("a b", 1).unwrap_err();
        assert!(err.to_string().contains("a b:1"));
    }

    #[test]
    fn resolve_carries_scope_limits_and_ceiling() {
        let agent = resolve(&identity_with(inline(), "\"*\"", "ci"), &[]);
        assert_eq!(agent.scope.repos, vec!["github.com/x/y"]);
        assert_eq!(agent.scope.paths, vec!["api/**"]);
        assert_eq!(agent.scope.read_paths, Some(vec!["**".to_string()]));
        assert_eq!(agent.limits.max_iterations, 7);
        assert_eq!(agent.limits.max_wall_clock_secs, 9);
        assert_eq!(agent.max_effect, EffectClass::Ci);
    }

    #[test]
    fn resolve_grants_exactly_the_scoped_set() {
        let available = vec![
            CapabilitySpec::new("a_tool", "d", serde_json::json!({}), EffectClass::None),
            CapabilitySpec::new("b_tool", "d", serde_json::json!({}), EffectClass::None),
        ];
        let agent = resolve(&identity_with(inline(), "\"a_tool\"", "none"), &available);
        assert_eq!(agent.capability_names(), vec!["a_tool"]);
        assert_eq!(agent.capabilities().len(), 1);
    }

    #[test]
    fn resolve_refuses_capabilities_scoped_to_another_identity() {
        let identity = identity_with(inline(), "\"*\"", "none");
        let scoped = testing::scoped(&identity, &[]);
        let other = AgentIdentity::from_toml_str(
            r#"
[identity]
name = "other"
description = "d"
loop = "inner"
model = "m"
system_prompt = { inline = "p" }
[scope]
repos = ["r"]
paths = ["**"]
max_effect = "none"
tools = ["*"]
[limits]
max_iterations = 1
max_wall_clock_secs = 1
max_concurrent = 1
"#,
            "other.toml",
        )
        .unwrap();
        let err = ResolvedAgent::resolve(
            &other,
            &scoped,
            "t",
            Endpoint::new("gw", 1).unwrap(),
            "img",
            "/s".into(),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::ScopeMismatch { .. }));
        assert_eq!(
            err.to_string(),
            "capabilities were scoped by a different identity than `other`"
        );
    }

    fn named(name: &str, tools: &str, effect: &str, paths: &str, read: &str) -> AgentIdentity {
        let toml = format!(
            r#"
[identity]
name = "{name}"
description = "d"
loop = "inner"
model = "m"
system_prompt = {{ inline = "p" }}
[scope]
repos = ["r"]
paths = {paths}
{read}
max_effect = "{effect}"
tools = [{tools}]
[limits]
max_iterations = 1
max_wall_clock_secs = 1
max_concurrent = 1
"#
        );
        AgentIdentity::from_toml_str(&toml, "x.toml").unwrap()
    }

    fn resolve_pair(
        scoped_by: &AgentIdentity,
        resolved: &AgentIdentity,
    ) -> Result<ResolvedAgent, ResolveError> {
        let scoped =
            ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), scoped_by);
        ResolvedAgent::resolve(
            resolved,
            &scoped,
            "t",
            Endpoint::new("gw", 1).unwrap(),
            "img",
            "/s".into(),
        )
    }

    #[test]
    fn same_name_narrower_local_identity_cannot_use_the_global_grant() {
        let global = named("implementer", "\"*\"", "workspace", "[\"**\"]", "");
        let local = named("implementer", "\"read_file\"", "none", "[\"**\"]", "");
        let err = resolve_pair(&global, &local).unwrap_err();
        assert!(matches!(err, ResolveError::ScopeMismatch { .. }));
        assert_eq!(
            err.to_string(),
            "capabilities were scoped by a different identity than `implementer`"
        );
        assert!(resolve_pair(&local, &local).is_ok());
    }

    #[test]
    fn same_name_identity_with_different_paths_or_read_paths_is_refused() {
        let base = named("implementer", "\"*\"", "workspace", "[\"**\"]", "");
        let narrower_paths = named("implementer", "\"*\"", "workspace", "[\"api/**\"]", "");
        let with_reads = named(
            "implementer",
            "\"*\"",
            "workspace",
            "[\"**\"]",
            "read_paths = [\"**\"]",
        );
        for other in [&narrower_paths, &with_reads] {
            assert!(matches!(
                resolve_pair(&base, other),
                Err(ResolveError::ScopeMismatch { .. })
            ));
        }
    }

    #[test]
    fn widened_grant_fails_closed() {
        let identity = named("a", "\"read_file\"", "none", "[\"**\"]", "");
        let mut scoped = testing::scoped(&identity, &[]);
        scoped.specs.push(CapabilitySpec::new(
            "write_file",
            "d",
            serde_json::json!({}),
            EffectClass::Workspace,
        ));
        let err = ResolvedAgent::resolve(
            &identity,
            &scoped,
            "t",
            Endpoint::new("gw", 1).unwrap(),
            "img",
            "/s".into(),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::GrantWiderThanIdentity { .. }));
        assert_eq!(
            err.to_string(),
            "capability `write_file` is wider than identity `a` allows"
        );
        scoped.specs[0] = CapabilitySpec::new(
            "read_file",
            "d",
            serde_json::json!({}),
            EffectClass::Workspace,
        );
        scoped.specs.truncate(1);
        assert!(matches!(
            ResolvedAgent::resolve(
                &identity,
                &scoped,
                "t",
                Endpoint::new("gw", 1).unwrap(),
                "img",
                "/s".into(),
            ),
            Err(ResolveError::GrantWiderThanIdentity { .. })
        ));
    }

    #[test]
    fn real_registry_grant_matches_the_identity_scope() {
        let identity = identity_with(
            inline(),
            "\"read_file\", \"cargo_*\", \"run_command\"",
            "workspace",
        );
        let scoped =
            ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
        assert_eq!(scoped.identity(), "a");
        for spec in scoped.specs() {
            assert!(identity.allows_tool(&spec.name), "{}", spec.name);
            assert!(identity.allows_effect(spec.effect), "{}", spec.name);
        }
        assert!(scoped.specs().iter().all(|s| s.name != "run_command"));
        assert!(scoped.specs().windows(2).all(|w| w[0].name < w[1].name));
    }

    #[test]
    fn middle_and_outer_loop_ceilings_grant_effectful_capabilities() {
        let available: Vec<CapabilitySpec> = [
            ("read_file", EffectClass::None),
            ("git_push", EffectClass::Repository),
            ("trigger_ci", EffectClass::Ci),
            ("deploy_sandbox", EffectClass::Sandbox),
            ("deploy_prod", EffectClass::Production),
        ]
        .into_iter()
        .map(|(n, e)| CapabilitySpec::new(n, "d", serde_json::json!({}), e))
        .collect();
        for (ceiling, expected) in [
            ("none", 1),
            ("repository", 2),
            ("ci", 3),
            ("sandbox", 4),
            ("production", 5),
        ] {
            let agent = resolve(&identity_with(inline(), "\"*\"", ceiling), &available);
            assert_eq!(agent.capabilities().len(), expected, "{ceiling}");
            assert!(agent
                .capabilities()
                .iter()
                .all(|c| c.effect <= agent.max_effect));
        }
    }

    #[test]
    fn missing_prompt_file_is_a_resolve_error() {
        let identity = identity_with("\"prompts/missing.md\"", "\"*\"", "none");
        let scoped = testing::scoped(&identity, &[]);
        let err = ResolvedAgent::resolve(
            &identity,
            &scoped,
            "t",
            Endpoint::new("gw", 1).unwrap(),
            "img",
            "/s".into(),
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("system prompt:"));
    }

    #[test]
    fn scoped_capabilities_describe_tools_with_their_effect() {
        let identity = identity_with(inline(), "\"write_file\"", "workspace");
        let scoped =
            ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), &identity);
        let write = scoped
            .specs()
            .iter()
            .find(|s| s.name == "write_file")
            .unwrap();
        assert_eq!(write.effect, EffectClass::Workspace);
        assert_eq!(write.parameters["type"], "object");
    }

    #[test]
    fn adapters_report_their_kind() {
        assert_eq!(Broken.kind(), HarnessKind::Pi);
        assert_eq!(PiAdapter.kind().as_str(), "pi");
    }

    #[test]
    fn harness_kind_round_trips_through_serde() {
        let json = serde_json::to_string(&HarnessKind::Pi).unwrap();
        assert_eq!(json, "\"pi\"");
        assert_eq!(
            serde_json::from_str::<HarnessKind>(&json).unwrap(),
            HarnessKind::Pi
        );
    }

    struct Broken;

    impl HarnessAdapter for Broken {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Pi
        }

        fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported> {
            Ok(LaunchPlan {
                image: "other".into(),
                files: vec![],
                argv: vec!["x".into()],
                env: vec![],
                mounts: vec![],
                network: PlanNetwork::None,
                stdin: vec![],
                exposed_capabilities: vec![],
                limits: agent.limits,
                scope: agent.scope.clone(),
            })
        }
    }

    #[test]
    fn launch_plan_rejects_a_misbehaving_adapter() {
        let agent = resolve(&identity_with(inline(), "\"*\"", "none"), &[]);
        let err = launch_plan(&Broken, &agent).unwrap_err();
        assert!(matches!(
            err,
            PlanError::Isolation(IsolationViolation::Image { .. })
        ));
        assert!(err.to_string().contains("other"));
    }

    #[test]
    fn launch_plan_surfaces_unsupported() {
        let agent = resolve(&identity_with(inline(), "\"*\"", "none"), &[]);
        let err = launch_plan(&PiAdapter, &agent).unwrap_err();
        assert!(matches!(
            err,
            PlanError::Unsupported(Unsupported::NoCapabilities { .. })
        ));
    }

    #[test]
    fn handshake_reports_unexpected_and_missing_tools() {
        let agent = resolve(
            &identity_with(inline(), "\"read_file\", \"search\"", "none"),
            &[
                CapabilitySpec::new("read_file", "d", serde_json::json!({}), EffectClass::None),
                CapabilitySpec::new("search", "d", serde_json::json!({}), EffectClass::None),
            ],
        );
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        let ok = ["search".to_string(), "read_file".to_string()];
        assert!(verify_runtime_tools(&plan, &ok).is_ok());
        let bad = [
            "read_file".to_string(),
            "bash".to_string(),
            "write".to_string(),
        ];
        let err = verify_runtime_tools(&plan, &bad).unwrap_err();
        assert_eq!(err.unexpected, vec!["bash", "write"]);
        assert_eq!(err.missing, vec!["search"]);
        assert!(err.to_string().contains("bash"));
        assert!(verify_runtime_tools(&plan, &[]).is_err());
    }

    #[test]
    fn pi_plan_carries_scope_and_limits_for_enforcement() {
        let agent = resolve(
            &identity_with(inline(), "\"deploy_prod\"", "production"),
            &[CapabilitySpec::new(
                "deploy_prod",
                "d",
                serde_json::json!({}),
                EffectClass::Production,
            )],
        );
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        assert_eq!(plan.limits, agent.limits);
        assert_eq!(plan.scope, agent.scope);
        assert_eq!(plan.exposed_capabilities, vec!["deploy_prod"]);
    }
}
