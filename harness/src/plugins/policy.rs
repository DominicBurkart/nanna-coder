use super::render::{PluginError, PluginSource, RenderedAgent};
use super::types::HostPolicy;
use crate::effects::EffectClass;
use crate::identity::config_dir_from;
use serde::Deserialize;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const POLICY_FILE: &str = "policy.toml";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyViolation {
    #[error("agent `{agent}` reaches effect {effect}, above the host cap {cap}")]
    EffectAboveCap {
        agent: String,
        effect: EffectClass,
        cap: EffectClass,
    },
    #[error("agent `{agent}` tool `{tool}` uses the network, which the host policy forbids")]
    NetworkForbidden { agent: String, tool: String },
    #[error("agent `{agent}` verb `{verb}` is not in the host allowed_verbs")]
    VerbNotAllowed { agent: String, verb: String },
}

pub fn apply_policy(rendered: &RenderedAgent, policy: &HostPolicy) -> Result<(), PolicyViolation> {
    if rendered.source != PluginSource::Repo {
        return Ok(());
    }
    let agent = rendered.name.clone();
    if rendered.max_effect > policy.max_effect {
        return Err(PolicyViolation::EffectAboveCap {
            agent,
            effect: rendered.max_effect,
            cap: policy.max_effect,
        });
    }
    for tool in &rendered.tools {
        if tool.effect > policy.max_effect {
            return Err(PolicyViolation::EffectAboveCap {
                agent,
                effect: tool.effect,
                cap: policy.max_effect,
            });
        }
        if tool.network && !policy.network {
            return Err(PolicyViolation::NetworkForbidden {
                agent,
                tool: tool.name.clone(),
            });
        }
        if let Some(verb) = &tool.verb {
            if !policy.allowed_verbs.contains(&verb.verb) {
                return Err(PolicyViolation::VerbNotAllowed {
                    agent,
                    verb: verb.verb.clone(),
                });
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct PolicyFile {
    #[serde(default)]
    repo_plugins: Option<HostPolicy>,
}

pub fn parse_policy(text: &str, file: &Path) -> Result<HostPolicy, PluginError> {
    let parsed: PolicyFile = toml::from_str(text).map_err(|e| PluginError::Parse {
        file: file.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(parsed.repo_plugins.unwrap_or_default())
}

pub fn load_policy(path: &Path) -> Result<HostPolicy, PluginError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_policy(&text, path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HostPolicy::default()),
        Err(e) => Err(PluginError::Io {
            file: path.display().to_string(),
            message: e.to_string(),
        }),
    }
}

pub fn policy_path_from(lookup: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    config_dir_from(lookup).map(|dir| dir.join(POLICY_FILE))
}

pub fn load_default_policy() -> Result<HostPolicy, PluginError> {
    match policy_path_from(&|key| std::env::var_os(key)) {
        Some(path) => load_policy(&path),
        None => Ok(HostPolicy::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::render::{RenderedLimits, RenderedTool, RenderedVerb};

    fn agent(source: PluginSource) -> RenderedAgent {
        RenderedAgent {
            name: "go-implementer".into(),
            description: "d".into(),
            system_prompt: "p".into(),
            dev_loop: crate::identity::DevLoop::Inner,
            model: "m".into(),
            repos: vec![],
            write_paths: vec![],
            read_paths: vec!["**".into()],
            protected: vec![],
            max_effect: EffectClass::Workspace,
            tools: vec![
                RenderedTool {
                    name: "read_file".into(),
                    effect: EffectClass::None,
                    network: false,
                    verb: None,
                },
                RenderedTool {
                    name: "check".into(),
                    effect: EffectClass::Workspace,
                    network: false,
                    verb: Some(RenderedVerb {
                        verb: "check".into(),
                        argv: vec!["go".into(), "vet".into()],
                        timeout_secs: 300,
                    }),
                },
            ],
            limits: RenderedLimits {
                max_iterations: 1,
                max_wall_clock_secs: 1,
                max_concurrent: 1,
            },
            source,
            plugin_version: "v".into(),
        }
    }

    fn permissive() -> HostPolicy {
        HostPolicy {
            max_effect: EffectClass::Workspace,
            network: false,
            allowed_verbs: vec!["check".into()],
        }
    }

    #[test]
    fn the_default_policy_rejects_a_repo_agent_that_executes() {
        let err = apply_policy(&agent(PluginSource::Repo), &HostPolicy::default()).unwrap_err();
        assert!(matches!(err, PolicyViolation::EffectAboveCap { .. }));
    }

    #[test]
    fn curated_and_host_agents_are_exempt() {
        assert!(apply_policy(&agent(PluginSource::Curated), &HostPolicy::default()).is_ok());
        assert!(apply_policy(&agent(PluginSource::Host), &HostPolicy::default()).is_ok());
    }

    #[test]
    fn a_repo_agent_within_the_policy_passes() {
        assert!(apply_policy(&agent(PluginSource::Repo), &permissive()).is_ok());
    }

    #[test]
    fn a_verb_outside_allowed_verbs_is_rejected() {
        let mut policy = permissive();
        policy.allowed_verbs.clear();
        let err = apply_policy(&agent(PluginSource::Repo), &policy).unwrap_err();
        assert_eq!(
            err,
            PolicyViolation::VerbNotAllowed {
                agent: "go-implementer".into(),
                verb: "check".into()
            }
        );
    }

    #[test]
    fn a_network_verb_needs_the_network_grant() {
        let mut a = agent(PluginSource::Repo);
        a.tools[1].network = true;
        let err = apply_policy(&a, &permissive()).unwrap_err();
        assert!(matches!(err, PolicyViolation::NetworkForbidden { .. }));
        let mut policy = permissive();
        policy.network = true;
        assert!(apply_policy(&a, &policy).is_ok());
    }

    #[test]
    fn an_effect_above_the_cap_is_rejected() {
        let mut a = agent(PluginSource::Repo);
        a.max_effect = EffectClass::Repository;
        let err = apply_policy(&a, &permissive()).unwrap_err();
        assert!(err.to_string().contains("above the host cap"));
    }

    #[test]
    fn a_tool_above_the_cap_is_rejected_even_under_a_low_agent_ceiling() {
        let mut a = agent(PluginSource::Repo);
        a.tools[0].effect = EffectClass::Ci;
        assert!(matches!(
            apply_policy(&a, &permissive()),
            Err(PolicyViolation::EffectAboveCap { .. })
        ));
    }

    #[test]
    fn the_repo_plugins_table_is_read_from_policy_toml() {
        let text = "[repo_plugins]\nmax_effect = \"workspace\"\nallowed_verbs = [\"check\"]\n";
        let policy = parse_policy(text, Path::new("policy.toml")).unwrap();
        assert_eq!(policy.max_effect, EffectClass::Workspace);
        assert_eq!(policy.allowed_verbs, ["check"]);
        assert!(!policy.network);
    }

    #[test]
    fn a_policy_without_the_table_is_the_narrowest() {
        let policy = parse_policy("[other]\nx = 1\n", Path::new("policy.toml")).unwrap();
        assert_eq!(policy, HostPolicy::default());
    }

    #[test]
    fn a_malformed_policy_is_an_error() {
        let err = parse_policy(
            "[repo_plugins]\nmax_effect = \"bogus\"\n",
            Path::new("p.toml"),
        );
        assert!(matches!(err, Err(PluginError::Parse { .. })));
        let err = parse_policy(
            "[repo_plugins]\nmax_effect = \"none\"\nwat = 1\n",
            Path::new("p"),
        );
        assert!(err.is_err());
    }

    #[test]
    fn a_missing_policy_file_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let policy = load_policy(&dir.path().join("policy.toml")).unwrap();
        assert_eq!(policy, HostPolicy::default());
    }

    #[test]
    fn a_policy_file_is_loaded_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        fs::write(
            &path,
            "[repo_plugins]\nmax_effect = \"ci\"\nnetwork = true\n",
        )
        .unwrap();
        let policy = load_policy(&path).unwrap();
        assert_eq!(policy.max_effect, EffectClass::Ci);
        assert!(policy.network);
    }

    #[test]
    fn the_policy_path_follows_the_config_dir() {
        let lookup = |key: &str| (key == "NANNA_CONFIG_DIR").then(|| "/etc/nanna".into());
        assert_eq!(
            policy_path_from(&lookup),
            Some(PathBuf::from("/etc/nanna/policy.toml"))
        );
        assert_eq!(policy_path_from(&|_: &str| None), None);
    }
}
