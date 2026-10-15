//! The isolation contract every [`LaunchPlan`] must satisfy, whatever runtime
//! it targets.

use super::{LaunchPlan, ResolvedAgent};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Where the broker socket appears inside the container. Fixed so that no
/// adapter chooses it.
pub const BROKER_SOCKET_CONTAINER_PATH: &str = "/nanna/broker.sock";

const FILE_ROOT: &str = "/nanna/";

const ALLOWED_ENV: [(&str, &str); 4] = [
    ("HOME", "/nanna/work"),
    ("PI_CODING_AGENT_DIR", "/nanna/pi"),
    ("NANNA_BROKER_SOCKET", BROKER_SOCKET_CONTAINER_PATH),
    ("NANNA_CAPABILITIES", "/nanna/pi/capabilities.json"),
];

/// A way a plan breaks the isolation contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IsolationViolation {
    /// The plan names a different agent than the one it was built for.
    #[error("plan agent `{plan}` differs from the agent `{agent}`")]
    AgentChanged {
        /// Agent name in the plan.
        plan: String,
        /// Agent the plan was built for.
        agent: String,
    },
    /// The plan starts from a different image than the agent was resolved for.
    #[error("plan image `{plan}` differs from the agent image `{agent}`")]
    Image {
        /// Image in the plan.
        plan: String,
        /// Image the agent was resolved with.
        agent: String,
    },
    /// The plan mounts a broker socket other than the agent's.
    #[error("plan mounts {path} but the agent's broker socket is different")]
    BrokerSocket {
        /// Socket path in the plan.
        path: PathBuf,
    },
    /// The plan's network endpoint is not the agent's model gateway.
    #[error("network endpoint {host}:{port} is not the model gateway")]
    Gateway {
        /// Host of the endpoint.
        host: String,
        /// Port of the endpoint.
        port: u16,
    },
    /// The plan exposes a capability the agent was not granted.
    #[error("capability `{name}` is not granted to the agent")]
    Capability {
        /// Capability name.
        name: String,
    },
    /// The plan sets an environment variable that is not on the exact
    /// (name, value) allowlist, or sets one twice.
    #[error("environment variable `{name}` is not allowed with that value")]
    EnvNotAllowed {
        /// Variable name.
        name: String,
    },
    /// The plan writes a file outside `/nanna/`, at the broker mount path, or
    /// at a path with `..`, a backslash or a NUL.
    #[error("file path {path} must be absolute under /nanna/ with no `..`, backslash or NUL")]
    FilePath {
        /// Offending path.
        path: PathBuf,
    },
    /// The plan writes the same path twice.
    #[error("file path {path} is written twice")]
    DuplicateFile {
        /// Offending path.
        path: PathBuf,
    },
    /// The plan's limits differ from the agent's, so a ceiling would go unenforced.
    #[error("plan limits differ from the agent's")]
    LimitsDropped,
    /// The plan's scope differs from the agent's, so the broker would not enforce it.
    #[error("plan scope differs from the agent's")]
    ScopeDropped,
    /// The plan's audited repository differs from the agent's.
    #[error("plan audited repository differs from the agent's")]
    RepoChanged,
    /// The plan has no command to run.
    #[error("plan has an empty argv")]
    EmptyArgv,
}

/// Verifies a [`LaunchPlan`] against the isolation contract.
///
/// The contract: the only mount is the agent's broker socket; the only
/// network endpoint is the agent's model gateway; the capabilities exposed
/// are a subset of those granted; environment variables are exact
/// (name, value) pairs from a fixed list; the agent's limits and scope are
/// carried unchanged; files are placed under `/nanna/` without duplicates; the
/// image is the one the agent resolved with.
pub struct IsolationPolicy;

impl IsolationPolicy {
    /// Every violation in `plan`.
    pub fn violations(plan: &LaunchPlan, agent: &ResolvedAgent) -> Vec<IsolationViolation> {
        let mut found = Vec::new();
        if plan.argv.is_empty() {
            found.push(IsolationViolation::EmptyArgv);
        }
        let mut seen_files = BTreeSet::new();
        for file in &plan.files {
            if !file_path_is_contained(&file.path) {
                found.push(IsolationViolation::FilePath {
                    path: file.path.clone(),
                });
            } else if !seen_files.insert(file.path.clone()) {
                found.push(IsolationViolation::DuplicateFile {
                    path: file.path.clone(),
                });
            }
        }
        let mut seen_env = BTreeSet::new();
        for (name, value) in &plan.env {
            let pinned = ALLOWED_ENV.contains(&(name.as_str(), value.as_str()));
            if !pinned || !seen_env.insert(name.clone()) {
                found.push(IsolationViolation::EnvNotAllowed { name: name.clone() });
            }
        }
        if plan.agent != agent.name() {
            found.push(IsolationViolation::AgentChanged {
                plan: plan.agent.clone(),
                agent: agent.name().to_string(),
            });
        }
        if plan.image != agent.image() {
            found.push(IsolationViolation::Image {
                plan: plan.image.clone(),
                agent: agent.image().to_string(),
            });
        }
        if plan.broker_socket != agent.broker_socket() {
            found.push(IsolationViolation::BrokerSocket {
                path: plan.broker_socket.clone(),
            });
        }
        if plan.gateway != *agent.endpoint() {
            found.push(IsolationViolation::Gateway {
                host: plan.gateway.host().to_string(),
                port: plan.gateway.port(),
            });
        }
        if plan.limits != agent.limits() {
            found.push(IsolationViolation::LimitsDropped);
        }
        if plan.scope != *agent.scope() {
            found.push(IsolationViolation::ScopeDropped);
        }
        if plan.audited_repo != agent.audited_repo() {
            found.push(IsolationViolation::RepoChanged);
        }
        let granted: BTreeSet<&str> = agent.capability_names().into_iter().collect();
        for name in plan
            .exposed_capabilities
            .iter()
            .filter(|n| !granted.contains(n.as_str()))
        {
            found.push(IsolationViolation::Capability { name: name.clone() });
        }
        found
    }

    /// `Ok` when the plan has no violations, otherwise the first one.
    pub fn check(plan: &LaunchPlan, agent: &ResolvedAgent) -> Result<(), IsolationViolation> {
        match Self::violations(plan, agent).into_iter().next() {
            Some(violation) => Err(violation),
            None => Ok(()),
        }
    }
}

fn file_path_is_contained(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let escapes = text.split('/').any(|part| part == "..");
    text != BROKER_SOCKET_CONTAINER_PATH
        && text.starts_with(FILE_ROOT)
        && text.len() > FILE_ROOT.len()
        && !text.contains('\\')
        && !text.contains('\0')
        && !escapes
}
