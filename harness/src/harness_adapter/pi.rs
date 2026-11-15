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
//! `--no-approve` is load-bearing: with project approval, a project-level
//! `.pi/SYSTEM.md` replaces the nanna system prompt. Unknown flags are a hard
//! error in pi, so a misspelt lockdown flag fails closed at start.
//!
//! Network: `container::NetworkPolicy::for_ceiling` governs the dev container
//! where broker-run tools execute, and still applies there. The agent
//! container is a different container whose only legitimate need is the model
//! gateway, so its plan is always exactly that one endpoint, whatever the
//! effect ceiling; there is no second ceiling-derived rule to drift.
//!
//! Pi writes `auth.json` and `models-store.json` into `PI_CODING_AGENT_DIR`,
//! so that directory must be writable; the executor cannot mount plan files
//! read-only.

use super::isolation::BROKER_SOCKET_CONTAINER_PATH;
use super::{HarnessAdapter, LaunchPlan, PlanFile, ResolvedAgent, Unsupported};
use std::path::PathBuf;

const AGENT_DIR: &str = "/nanna/pi";
const WORK_DIR: &str = "/nanna/work";
const PROVIDER: &str = "nanna";
const MAX_CAPABILITY_NAME: usize = 64;

/// Translates a [`ResolvedAgent`] into a pi RPC launch plan.
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
                "baseUrl": format!(
                    "http://{}:{}/v1",
                    agent.endpoint().host(),
                    agent.endpoint().port()
                ),
                "api": "openai-completions",
                "apiKey": "nanna",
                "models": [{ "id": agent.model() }],
            }
        }
    })
    .to_string()
}

impl HarnessAdapter for PiAdapter {
    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported> {
        if agent.capabilities().is_empty() {
            return Err(Unsupported::NoCapabilities {
                agent: agent.name().to_string(),
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
            "message": agent.task_prompt(),
        })
        .to_string();

        Ok(LaunchPlan {
            image: agent.image().to_string(),
            files: vec![
                file("models.json", models_json(agent)),
                file("SYSTEM.md", agent.system_prompt().to_string()),
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
                agent.model(),
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
            broker_socket: agent.broker_socket().to_path_buf(),
            gateway: agent.endpoint().clone(),
            stdin: vec![prompt],
            exposed_capabilities: agent
                .capability_names()
                .into_iter()
                .map(String::from)
                .collect(),
            limits: agent.limits(),
            scope: agent.scope().clone(),
            audited_repo: agent.audited_repo().to_string(),
        })
    }
}
