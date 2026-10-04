//! Container integration test for identity-scoped `run_command`.
//!
//! An identity whose ceiling is below `repository` gets a dev container
//! started with [`NetworkPolicy::Disabled`]; this test drives the real
//! policy -> runtime flag -> `run_command` chain and checks the container
//! has no interface but loopback and cannot connect anywhere. A control
//! container under a `repository` ceiling keeps its network interface.
//!
//! Like the other container tests, it skips (loudly) when no container
//! runtime is available or the image cannot be pulled.

use harness::container::{
    detect_runtime, start_container_with_fallback, ContainerConfig, ContainerError,
    ContainerHandle, NetworkPolicy,
};
use harness::effects::EffectClass;
use harness::identity::AgentIdentity;
use harness::tools::create_container_tool_registry_for;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const IMAGE: &str = "docker.io/library/alpine:latest";

fn identity(ceiling: EffectClass) -> AgentIdentity {
    let toml = format!(
        r#"
[identity]
name = "shell-runner"
description = "Runs commands in the dev container."
loop = "inner"
model = "gemma4:e4b"
system_prompt = {{ inline = "Run commands." }}

[scope]
repos = []
paths = ["**"]
max_effect = "{ceiling}"
tools = ["run_command", "read_file"]

[limits]
max_iterations = 10
max_wall_clock_secs = 60
max_concurrent = 1
"#
    );
    AgentIdentity::from_toml_str(&toml, "shell-runner.toml").unwrap()
}

async fn start(network: NetworkPolicy) -> Option<Arc<ContainerHandle>> {
    let config = ContainerConfig {
        base_image: IMAGE.to_string(),
        test_image: None,
        container_name: format!("nanna-rbac-net-{}", uuid::Uuid::new_v4()),
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(2),
        health_check_timeout: Duration::from_secs(2),
        env_vars: vec![],
        additional_args: vec!["-i".to_string(), "-t".to_string()],
        network,
        read_only_mounts: vec![],
    };
    match start_container_with_fallback(&config).await {
        Ok(handle) => Some(Arc::new(handle)),
        Err(ContainerError::ImageNotFound { image, suggestion }) => {
            eprintln!("SKIPPED: image {image} unavailable: {suggestion}");
            None
        }
        Err(e) => panic!("container start failed: {e}"),
    }
}

async fn interfaces(registry: &harness::tools::ToolRegistry) -> Vec<String> {
    let listing = registry
        .execute("run_command", json!({ "command": "ls /sys/class/net" }))
        .await
        .expect("run_command should execute");
    assert_eq!(listing["success"], true, "{listing}");
    listing["stdout"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn run_command_under_a_workspace_ceiling_cannot_reach_the_network() {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let identity = identity(EffectClass::Workspace);
    let network = NetworkPolicy::for_ceiling(identity.scope.max_effect);
    assert_eq!(network, NetworkPolicy::Disabled);

    let Some(handle) = start(network).await else {
        return;
    };
    let registry =
        create_container_tool_registry_for(workspace.path(), handle, "/", &identity).unwrap();
    assert!(registry.get_tool("run_command").is_some());
    assert!(registry.get_tool("github_pr_status").is_none());

    assert_eq!(interfaces(&registry).await, vec!["lo".to_string()]);

    let fetch = registry
        .execute(
            "run_command",
            json!({ "command": "wget -T 3 -O /dev/null http://1.1.1.1" }),
        )
        .await
        .expect("run_command should execute even when the network is unreachable");
    assert_eq!(fetch["success"], false, "{fetch}");
    assert_eq!(registry.denial_count(), 0);
}

#[tokio::test]
async fn run_command_under_a_repository_ceiling_keeps_its_network_interface() {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let identity = identity(EffectClass::Repository);
    let network = NetworkPolicy::for_ceiling(identity.scope.max_effect);
    assert_eq!(network, NetworkPolicy::Enabled);

    let Some(handle) = start(network).await else {
        return;
    };
    let registry =
        create_container_tool_registry_for(workspace.path(), handle, "/", &identity).unwrap();

    let interfaces = interfaces(&registry).await;
    assert!(
        interfaces.iter().any(|name| name != "lo"),
        "expected a non-loopback interface, got {interfaces:?}"
    );
}
