//! Adapter that runs an agent on the pi coding agent in RPC mode.
//!
//! Pi ships with no permission system and full host access, so isolation does
//! not rest on it. The launch plan disables every built-in tool, extension
//! discovery, skills, prompt templates, context files, MCP and project
//! approval, gives the runtime an empty working directory, and loads one
//! static extension that forwards each granted capability to the broker
//! socket. The capability list is data (`capabilities.json`), never generated
//! code.
//!
//! Network: `container::NetworkPolicy::for_ceiling` governs the dev container
//! where broker-run tools execute, and still applies there. The agent
//! container is a different container whose only legitimate need is the model
//! gateway, so its plan is always exactly that one endpoint, whatever the
//! effect ceiling; there is no second ceiling-derived rule to drift.

use super::isolation::BROKER_SOCKET_CONTAINER_PATH;
use super::{
    HarnessAdapter, HarnessKind, LaunchPlan, Mount, PlanFile, PlanNetwork, ResolvedAgent,
    Unsupported,
};
use std::path::PathBuf;

const AGENT_DIR: &str = "/nanna/pi";
const WORK_DIR: &str = "/nanna/work";
const PROVIDER: &str = "nanna";
const MAX_CAPABILITY_NAME: usize = 64;

/// Translates a [`ResolvedAgent`] into a pi RPC launch plan.
///
/// ```
/// use harness::harness_adapter::{HarnessAdapter, HarnessKind, PiAdapter};
///
/// assert_eq!(PiAdapter::default().kind(), HarnessKind::Pi);
/// ```
#[derive(Debug, Clone, Default)]
pub struct PiAdapter;

fn name_is_representable(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !first_ok || !rest_ok {
        return Err("must match [a-z][a-z0-9_]*".to_string());
    }
    if name.len() > MAX_CAPABILITY_NAME {
        return Err(format!("longer than {MAX_CAPABILITY_NAME} characters"));
    }
    Ok(())
}

fn models_json(agent: &ResolvedAgent) -> String {
    serde_json::json!({
        "providers": {
            PROVIDER: {
                "baseUrl": format!("http://{}:{}/v1", agent.endpoint.host(), agent.endpoint.port()),
                "api": "openai-completions",
                "apiKey": "nanna",
                "models": [{ "id": agent.model }],
            }
        }
    })
    .to_string()
}

impl HarnessAdapter for PiAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Pi
    }

    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported> {
        if agent.capabilities().is_empty() {
            return Err(Unsupported::NoCapabilities {
                agent: agent.name.clone(),
            });
        }
        if agent.model.trim().is_empty() {
            return Err(Unsupported::NoModel {
                agent: agent.name.clone(),
            });
        }
        for capability in agent.capabilities() {
            name_is_representable(&capability.name).map_err(|reason| {
                Unsupported::CapabilityName {
                    name: capability.name.clone(),
                    reason,
                }
            })?;
        }

        let capabilities = serde_json::to_string(agent.capabilities())
            .expect("capability specs are plain JSON data");
        let file = |name: &str, contents: String| PlanFile {
            path: PathBuf::from(format!("{AGENT_DIR}/{name}")),
            contents,
        };
        let extension_path = format!("{AGENT_DIR}/extensions/nanna-capabilities.ts");
        let prompt = serde_json::json!({
            "id": "task",
            "type": "prompt",
            "message": agent.task_prompt,
        })
        .to_string();

        Ok(LaunchPlan {
            image: agent.image.clone(),
            files: vec![
                file("models.json", models_json(agent)),
                file("SYSTEM.md", agent.system_prompt.clone()),
                file("capabilities.json", capabilities),
                PlanFile {
                    path: PathBuf::from(&extension_path),
                    contents: super::pi_extension::SOURCE.to_string(),
                },
            ],
            argv: [
                "pi",
                "--mode",
                "rpc",
                "--no-session",
                "--provider",
                PROVIDER,
                "--model",
                agent.model.as_str(),
                "--no-builtin-tools",
                "--no-extensions",
                "--extension",
                extension_path.as_str(),
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
                "--no-mcp",
                "--no-approve",
                "--offline",
            ]
            .map(String::from)
            .to_vec(),
            env: vec![
                ("PI_CODING_AGENT_DIR".into(), AGENT_DIR.into()),
                ("HOME".into(), WORK_DIR.into()),
                (
                    "NANNA_BROKER_SOCKET".into(),
                    BROKER_SOCKET_CONTAINER_PATH.into(),
                ),
                (
                    "NANNA_CAPABILITIES".into(),
                    format!("{AGENT_DIR}/capabilities.json"),
                ),
            ],
            mounts: vec![Mount {
                host: agent.broker_socket.clone(),
                container: PathBuf::from(BROKER_SOCKET_CONTAINER_PATH),
                read_only: false,
            }],
            network: PlanNetwork::Only(vec![agent.endpoint.clone()]),
            stdin: vec![prompt],
            exposed_capabilities: agent
                .capability_names()
                .into_iter()
                .map(String::from)
                .collect(),
            limits: agent.limits,
            scope: agent.scope.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use crate::harness_adapter::{launch_plan, CapabilitySpec, Endpoint, IsolationPolicy};
    use crate::identity::AgentIdentity;
    use proptest::prelude::*;

    fn identity(tools: &str, effect: &str) -> AgentIdentity {
        let toml = format!(
            r#"
[identity]
name = "a"
description = "d"
loop = "inner"
model = "gemma4:e4b"
system_prompt = {{ inline = "Be careful." }}
[scope]
repos = ["r"]
paths = ["**"]
max_effect = "{effect}"
tools = [{tools}]
[limits]
max_iterations = 1
max_wall_clock_secs = 1
max_concurrent = 1
"#
        );
        AgentIdentity::from_toml_str(&toml, "a.toml").unwrap()
    }

    fn catalog() -> Vec<CapabilitySpec> {
        [
            ("read_file", EffectClass::None),
            ("write_file", EffectClass::Workspace),
            ("cargo_check", EffectClass::Workspace),
            ("git_push", EffectClass::Repository),
            ("deploy", EffectClass::Production),
        ]
        .into_iter()
        .map(|(n, e)| {
            CapabilitySpec::new(
                n,
                format!("{n} tool"),
                serde_json::json!({"type": "object"}),
                e,
            )
        })
        .collect()
    }

    fn resolve(identity: &AgentIdentity, available: &[CapabilitySpec]) -> ResolvedAgent {
        let scoped = crate::harness_adapter::testing::scoped(identity, available);
        ResolvedAgent::resolve(
            identity,
            &scoped,
            "do the thing",
            Endpoint::new("model-gateway", 11434).unwrap(),
            "nanna-agent:pi",
            "/run/nanna/broker.sock".into(),
        )
        .unwrap()
    }

    #[test]
    fn plan_disables_every_ambient_pi_feature() {
        let agent = resolve(&identity("\"*\"", "workspace"), &catalog());
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        for flag in [
            "--no-builtin-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--no-mcp",
            "--no-approve",
            "--no-session",
        ] {
            assert!(plan.argv.iter().any(|a| a == flag), "missing {flag}");
        }
        assert_eq!(plan.argv[..3], ["pi", "--mode", "rpc"]);
    }

    #[test]
    fn plan_exposes_exactly_the_granted_capabilities() {
        let agent = resolve(
            &identity("\"read_file\", \"cargo_*\"", "workspace"),
            &catalog(),
        );
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        assert_eq!(plan.exposed_capabilities, vec!["cargo_check", "read_file"]);
        let file = plan
            .files
            .iter()
            .find(|f| f.path.ends_with("capabilities.json"))
            .unwrap();
        let names: Vec<String> = serde_json::from_str::<Vec<serde_json::Value>>(&file.contents)
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, plan.exposed_capabilities);
    }

    #[test]
    fn effect_ceiling_withholds_higher_effect_tools() {
        let agent = resolve(&identity("\"*\"", "workspace"), &catalog());
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        assert!(!plan.exposed_capabilities.contains(&"git_push".to_string()));
        assert!(!plan.exposed_capabilities.contains(&"deploy".to_string()));
    }

    #[test]
    fn models_json_points_only_at_the_gateway() {
        let agent = resolve(&identity("\"read_file\"", "none"), &catalog());
        let plan = launch_plan(&PiAdapter, &agent).unwrap();
        let file = plan
            .files
            .iter()
            .find(|f| f.path.ends_with("models.json"))
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
        assert_eq!(
            json["providers"]["nanna"]["baseUrl"],
            "http://model-gateway:11434/v1"
        );
        assert_eq!(json["providers"]["nanna"]["models"][0]["id"], "gemma4:e4b");
        assert_eq!(
            plan.network,
            PlanNetwork::Only(vec![Endpoint::new("model-gateway", 11434).unwrap()])
        );
    }

    #[test]
    fn agent_network_is_exactly_the_gateway_at_every_ceiling() {
        let gateway = Endpoint::new("model-gateway", 11434).unwrap();
        for ceiling in crate::effects::EffectClass::ALL {
            let agent = resolve(&identity("\"read_file\"", ceiling.as_str()), &catalog());
            let plan = launch_plan(&PiAdapter, &agent).unwrap();
            assert_eq!(
                plan.network,
                PlanNetwork::Only(vec![gateway.clone()]),
                "{ceiling}"
            );
        }
    }

    #[test]
    fn task_prompt_travels_as_an_rpc_prompt_line() {
        let agent = resolve(&identity("\"read_file\"", "none"), &catalog());
        let plan = PiAdapter.plan(&agent).unwrap();
        let line: serde_json::Value = serde_json::from_str(&plan.stdin[0]).unwrap();
        assert_eq!(line["type"], "prompt");
        assert_eq!(line["message"], "do the thing");
    }

    #[test]
    fn system_prompt_and_extension_are_written() {
        let agent = resolve(&identity("\"read_file\"", "none"), &catalog());
        let plan = PiAdapter.plan(&agent).unwrap();
        let system = plan
            .files
            .iter()
            .find(|f| f.path.ends_with("SYSTEM.md"))
            .unwrap();
        assert_eq!(system.contents, "Be careful.");
        let ext = plan
            .files
            .iter()
            .find(|f| f.path.ends_with("nanna-capabilities.ts"))
            .unwrap();
        assert!(ext.contents.contains("registerTool"));
    }

    #[test]
    fn mounts_only_the_broker_socket() {
        let agent = resolve(&identity("\"read_file\"", "none"), &catalog());
        let plan = PiAdapter.plan(&agent).unwrap();
        assert_eq!(plan.mounts.len(), 1);
        assert_eq!(plan.mounts[0].host, PathBuf::from("/run/nanna/broker.sock"));
    }

    #[test]
    fn no_capabilities_is_unsupported() {
        let agent = resolve(&identity("\"nothing\"", "none"), &catalog());
        assert_eq!(
            PiAdapter.plan(&agent),
            Err(Unsupported::NoCapabilities { agent: "a".into() })
        );
    }

    #[test]
    fn empty_model_is_unsupported() {
        let mut agent = resolve(&identity("\"read_file\"", "none"), &catalog());
        agent.model = "  ".into();
        assert_eq!(
            PiAdapter.plan(&agent),
            Err(Unsupported::NoModel { agent: "a".into() })
        );
    }

    #[test]
    fn unrepresentable_capability_names_are_unsupported() {
        for name in ["Read", "read-file", "1read", "x\"y", &"a".repeat(65)] {
            let available = vec![CapabilitySpec::new(
                name,
                "d",
                serde_json::json!({}),
                EffectClass::None,
            )];
            let identity = identity("\"*\"", "none");
            let scoped = crate::harness_adapter::testing::scoped(&identity, &available);
            let agent = ResolvedAgent::resolve(
                &identity,
                &scoped,
                "t",
                Endpoint::new("gw", 1).unwrap(),
                "img",
                "/s".into(),
            )
            .unwrap();
            let result = PiAdapter.plan(&agent);
            let hit = matches!(result, Err(Unsupported::CapabilityName { .. }));
            assert!(hit, "{name}");
        }
    }

    #[test]
    fn narrower_identity_never_widens_the_plan() {
        let wide = resolve(&identity("\"*\"", "workspace"), &catalog());
        let narrow = resolve(&identity("\"read_file\"", "none"), &catalog());
        let wide_plan = PiAdapter.plan(&wide).unwrap();
        let narrow_plan = PiAdapter.plan(&narrow).unwrap();
        for name in &narrow_plan.exposed_capabilities {
            assert!(wide_plan.exposed_capabilities.contains(name));
        }
        assert!(narrow_plan.exposed_capabilities.len() < wide_plan.exposed_capabilities.len());
    }

    proptest! {
        #[test]
        fn plans_always_satisfy_isolation(
            mask in proptest::collection::vec(any::<bool>(), 5),
            effect in prop::sample::select(vec!["none", "workspace", "repository", "ci", "sandbox", "production"]),
        ) {
            let patterns: Vec<String> = catalog()
                .iter()
                .zip(&mask)
                .filter(|(_, keep)| **keep)
                .map(|(c, _)| format!("\"{}\"", c.name))
                .collect();
            prop_assume!(!patterns.is_empty());
            let agent = resolve(&identity(&patterns.join(","), effect), &catalog());
            if let Ok(plan) = PiAdapter.plan(&agent) {
                prop_assert_eq!(IsolationPolicy::check(&plan, &agent), Ok(()));
                for name in &plan.exposed_capabilities {
                    let spec = catalog().into_iter().find(|c| &c.name == name).unwrap();
                    prop_assert!(spec.effect <= agent.max_effect);
                }
            }
        }
    }
}
