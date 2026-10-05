//! The isolation contract every [`LaunchPlan`] must satisfy, whatever runtime
//! it targets.

use super::{LaunchPlan, PlanNetwork, ResolvedAgent};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Where the broker socket appears inside the container. Fixed so that no
/// adapter chooses it.
pub const BROKER_SOCKET_CONTAINER_PATH: &str = "/nanna/broker.sock";

const ALLOWED_ENV_NAMES: [&str; 5] = [
    "HOME",
    "LANG",
    "PI_CODING_AGENT_DIR",
    "NANNA_BROKER_SOCKET",
    "NANNA_CAPABILITIES",
];
const SECRET_MARKERS: [&str; 6] = ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "COOKIE"];

/// A way a plan breaks the isolation contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IsolationViolation {
    /// The plan starts from a different image than the agent was resolved for.
    #[error("plan image `{plan}` differs from the agent image `{agent}`")]
    Image {
        /// Image in the plan.
        plan: String,
        /// Image the agent was resolved with.
        agent: String,
    },
    /// The plan mounts something other than the broker socket.
    #[error("mount {host} -> {container} is not the broker socket")]
    Mount {
        /// Host side of the mount.
        host: PathBuf,
        /// Container side of the mount.
        container: PathBuf,
    },
    /// The plan reaches an endpoint other than the model gateway.
    #[error("network endpoint {host}:{port} is not the model gateway")]
    Network {
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
    /// The plan sets an environment variable outside the allowlist.
    #[error("environment variable `{name}` is not on the allowlist")]
    EnvNotAllowed {
        /// Variable name.
        name: String,
    },
    /// The plan writes a file at a non-absolute or escaping path.
    #[error("file path {path} must be absolute and free of `..`")]
    FilePath {
        /// Offending path.
        path: PathBuf,
    },
    /// The plan's limits differ from the agent's, so a ceiling would go unenforced.
    #[error("plan limits differ from the agent's")]
    LimitsDropped,
    /// The plan's scope differs from the agent's, so the broker would not enforce it.
    #[error("plan scope differs from the agent's")]
    ScopeDropped,
    /// The plan has no command to run.
    #[error("plan has an empty argv")]
    EmptyArgv,
}

/// Verifies a [`LaunchPlan`] against the isolation contract.
///
/// The contract: the only mount is the broker socket; the only network
/// endpoint is the agent's model gateway; the capabilities exposed are a
/// subset of those granted; environment names are an exact-name allowlist
/// (`HOME`, `LANG`, `PI_CODING_AGENT_DIR`, `NANNA_BROKER_SOCKET`,
/// `NANNA_CAPABILITIES`) that must also pass a secret-marker check; the agent's limits and
/// scope are carried unchanged; files are placed
/// at absolute paths without `..`; the image is the one the agent resolved
/// with.
///
/// ```
/// use harness::harness_adapter::{IsolationPolicy, IsolationViolation, LaunchPlan, PlanNetwork};
///
/// let plan = LaunchPlan {
///     image: "img".into(),
///     files: vec![],
///     argv: vec![],
///     env: vec![],
///     mounts: vec![],
///     network: PlanNetwork::None,
///     stdin: vec![],
///     exposed_capabilities: vec![],
///     limits: harness::identity::LimitsSection { max_iterations: 1, max_wall_clock_secs: 1, max_concurrent: 1 },
///     scope: harness::harness_adapter::ScopeGrant { repos: vec![], paths: vec![], read_paths: None },
/// };
/// assert_eq!(IsolationPolicy::violations(&plan, None).first(), Some(&IsolationViolation::EmptyArgv));
/// ```
pub struct IsolationPolicy;

impl IsolationPolicy {
    /// Every violation in `plan`. When `agent` is `None`, only the checks that
    /// need no agent context run.
    pub fn violations(plan: &LaunchPlan, agent: Option<&ResolvedAgent>) -> Vec<IsolationViolation> {
        let mut found = Vec::new();
        if plan.argv.is_empty() {
            found.push(IsolationViolation::EmptyArgv);
        }
        for file in &plan.files {
            if !path_is_contained(&file.path) {
                found.push(IsolationViolation::FilePath {
                    path: file.path.clone(),
                });
            }
        }
        for (name, _) in &plan.env {
            if !env_allowed(name) {
                found.push(IsolationViolation::EnvNotAllowed { name: name.clone() });
            }
        }
        let Some(agent) = agent else {
            return found;
        };
        if plan.image != agent.image {
            found.push(IsolationViolation::Image {
                plan: plan.image.clone(),
                agent: agent.image.clone(),
            });
        }
        for mount in &plan.mounts {
            let is_broker = mount.host == agent.broker_socket
                && mount.container == Path::new(BROKER_SOCKET_CONTAINER_PATH);
            if !is_broker {
                found.push(IsolationViolation::Mount {
                    host: mount.host.clone(),
                    container: mount.container.clone(),
                });
            }
        }
        if let PlanNetwork::Only(endpoints) = &plan.network {
            for endpoint in endpoints.iter().filter(|e| **e != agent.endpoint) {
                found.push(IsolationViolation::Network {
                    host: endpoint.host().to_string(),
                    port: endpoint.port(),
                });
            }
        }
        if plan.limits != agent.limits {
            found.push(IsolationViolation::LimitsDropped);
        }
        if plan.scope != agent.scope {
            found.push(IsolationViolation::ScopeDropped);
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
        match Self::violations(plan, Some(agent)).into_iter().next() {
            Some(violation) => Err(violation),
            None => Ok(()),
        }
    }
}

fn path_is_contained(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir))
}

fn env_allowed(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ALLOWED_ENV_NAMES.contains(&name) && !SECRET_MARKERS.iter().any(|m| upper.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use crate::harness_adapter::{CapabilitySpec, Endpoint, Mount, PlanFile};
    use crate::identity::AgentIdentity;

    fn agent() -> ResolvedAgent {
        let toml = r#"
[identity]
name = "a"
description = "d"
loop = "inner"
model = "m"
system_prompt = { inline = "p" }
[scope]
repos = ["r"]
paths = ["**"]
max_effect = "workspace"
tools = ["read_file"]
[limits]
max_iterations = 1
max_wall_clock_secs = 1
max_concurrent = 1
"#;
        let identity = AgentIdentity::from_toml_str(toml, "a.toml").unwrap();
        let available = vec![CapabilitySpec::new(
            "read_file",
            "r",
            serde_json::json!({}),
            EffectClass::None,
        )];
        ResolvedAgent::resolve(
            &identity,
            &available,
            "t",
            Endpoint::new("gw", 11434).unwrap(),
            "img",
            "/host/broker.sock".into(),
        )
        .unwrap()
    }

    fn good_plan() -> LaunchPlan {
        LaunchPlan {
            image: "img".into(),
            files: vec![PlanFile {
                path: "/nanna/x".into(),
                contents: String::new(),
            }],
            argv: vec!["run".into()],
            env: vec![("PI_CODING_AGENT_DIR".into(), "/nanna/pi".into())],
            mounts: vec![Mount {
                host: "/host/broker.sock".into(),
                container: BROKER_SOCKET_CONTAINER_PATH.into(),
                read_only: false,
            }],
            network: PlanNetwork::Only(vec![Endpoint::new("gw", 11434).unwrap()]),
            stdin: vec![],
            exposed_capabilities: vec!["read_file".into()],
            limits: agent().limits,
            scope: agent().scope,
        }
    }

    #[test]
    fn conforming_plan_passes() {
        assert_eq!(IsolationPolicy::check(&good_plan(), &agent()), Ok(()));
    }

    #[test]
    fn no_network_passes() {
        let mut plan = good_plan();
        plan.network = PlanNetwork::None;
        assert_eq!(IsolationPolicy::check(&plan, &agent()), Ok(()));
    }

    #[test]
    fn rejects_other_image() {
        let mut plan = good_plan();
        plan.image = "other".into();
        assert!(matches!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Image { .. })
        ));
    }

    #[test]
    fn rejects_workspace_mount() {
        let mut plan = good_plan();
        plan.mounts.push(Mount {
            host: "/home/u/repo".into(),
            container: "/work".into(),
            read_only: true,
        });
        assert!(matches!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Mount { .. })
        ));
    }

    #[test]
    fn rejects_broker_socket_at_wrong_container_path() {
        let mut plan = good_plan();
        plan.mounts[0].container = "/elsewhere.sock".into();
        assert!(matches!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Mount { .. })
        ));
    }

    #[test]
    fn rejects_foreign_socket_on_the_broker_path() {
        let mut plan = good_plan();
        plan.mounts[0].host = "/var/run/docker.sock".into();
        assert!(matches!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Mount { .. })
        ));
    }

    #[test]
    fn rejects_extra_network_endpoint() {
        let mut plan = good_plan();
        plan.network = PlanNetwork::Only(vec![
            Endpoint::new("gw", 11434).unwrap(),
            Endpoint::new("api.github.com", 443).unwrap(),
        ]);
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Network {
                host: "api.github.com".into(),
                port: 443
            })
        );
    }

    #[test]
    fn rejects_ungranted_capability() {
        let mut plan = good_plan();
        plan.exposed_capabilities.push("bash".into());
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::Capability {
                name: "bash".into()
            })
        );
    }

    #[test]
    fn exposing_fewer_capabilities_than_granted_is_allowed() {
        let mut plan = good_plan();
        plan.exposed_capabilities.clear();
        assert_eq!(IsolationPolicy::check(&plan, &agent()), Ok(()));
    }

    #[test]
    fn rejects_env_outside_the_allowlist() {
        for name in [
            "GITHUB_TOKEN",
            "api_key",
            "PATH",
            "AWS_REGION",
            "SSH_AUTH_SOCK",
            "NANNA",
            "LD_PRELOAD",
            "NANNA_GITHUB_TOKEN",
            "NANNA_API_KEY",
            "NANNA_ANYTHING",
        ] {
            let mut plan = good_plan();
            plan.env.push((name.into(), "x".into()));
            assert!(
                matches!(
                    IsolationPolicy::check(&plan, &agent()),
                    Err(IsolationViolation::EnvNotAllowed { .. })
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn accepts_allowlisted_env() {
        let mut plan = good_plan();
        plan.env = [
            "HOME",
            "LANG",
            "PI_CODING_AGENT_DIR",
            "NANNA_BROKER_SOCKET",
            "NANNA_CAPABILITIES",
        ]
        .iter()
        .map(|n| (n.to_string(), "x".to_string()))
        .collect();
        assert_eq!(IsolationPolicy::check(&plan, &agent()), Ok(()));
    }

    #[test]
    fn rejects_dropped_limits() {
        for tweak in [0, 1] {
            let mut plan = good_plan();
            if tweak == 0 {
                plan.limits.max_iterations += 1;
            } else {
                plan.limits.max_wall_clock_secs += 1;
            }
            assert_eq!(
                IsolationPolicy::check(&plan, &agent()),
                Err(IsolationViolation::LimitsDropped)
            );
        }
    }

    #[test]
    fn rejects_dropped_scope() {
        let mut plan = good_plan();
        plan.scope.paths.clear();
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::ScopeDropped)
        );
        let mut plan = good_plan();
        plan.scope.read_paths = Some(vec![]);
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::ScopeDropped)
        );
        let mut plan = good_plan();
        plan.scope.repos.push("other".into());
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::ScopeDropped)
        );
    }

    #[test]
    fn rejects_escaping_or_relative_file_paths() {
        for path in ["relative/x", "/nanna/../etc/passwd"] {
            let mut plan = good_plan();
            plan.files[0].path = path.into();
            assert!(
                matches!(
                    IsolationPolicy::check(&plan, &agent()),
                    Err(IsolationViolation::FilePath { .. })
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_empty_argv() {
        let mut plan = good_plan();
        plan.argv.clear();
        assert_eq!(
            IsolationPolicy::check(&plan, &agent()),
            Err(IsolationViolation::EmptyArgv)
        );
    }

    #[test]
    fn violations_reports_all_and_context_free_subset() {
        let mut plan = good_plan();
        plan.argv.clear();
        plan.image = "other".into();
        plan.env.push(("API_TOKEN".into(), "x".into()));
        assert_eq!(IsolationPolicy::violations(&plan, Some(&agent())).len(), 3);
        assert_eq!(IsolationPolicy::violations(&plan, None).len(), 2);
    }
}
