//! Container integration test for protected paths.
//!
//! The dev container gets every existing protected root of the worktree as
//! a read-only bind mount shadowing the writable `/workspace` mount, and
//! Nanna's own configuration directory is never mounted. This test starts a
//! real container from that configuration and checks, through
//! `run_command`, that the configuration directory is invisible, that
//! writes, creations and deletions under a protected root are refused, and
//! that an ordinary file in the worktree is still writable.
//!
//! The writable workspace mount carries `:z` so the control write succeeds
//! on SELinux-enforcing hosts; the read-only mounts need no label because
//! the `ro` flag refuses the write first.
//!
//! Like the other container tests, it skips (loudly) when no container
//! runtime is available or the image cannot be pulled.

use harness::container::{
    detect_runtime, start_container_with_fallback, ContainerConfig, ContainerError,
    ContainerHandle, NetworkPolicy, ReadOnlyMount,
};
use harness::protected::ProtectedPaths;
use harness::tools::{create_container_tool_registry, ToolRegistry, CONTAINER_WORKSPACE_DIR};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const IMAGE: &str = "docker.io/library/alpine:latest";

async fn start(workspace: &Path, mounts: Vec<ReadOnlyMount>) -> Option<Arc<ContainerHandle>> {
    let workspace_mount = format!("-v={}:{CONTAINER_WORKSPACE_DIR}:z", workspace.display());
    let config = ContainerConfig {
        base_image: IMAGE.to_string(),
        test_image: None,
        container_name: format!("nanna-protected-{}", uuid::Uuid::new_v4()),
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(2),
        health_check_timeout: Duration::from_secs(2),
        env_vars: vec![],
        additional_args: vec!["-i".to_string(), "-t".to_string(), workspace_mount],
        network: NetworkPolicy::Disabled,
        read_only_mounts: mounts,
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

async fn run(registry: &ToolRegistry, command: &str) -> Value {
    registry
        .execute("run_command", json!({ "command": command }))
        .await
        .expect("run_command should execute")
}

async fn assert_refused(registry: &ToolRegistry, command: &str) {
    let result = run(registry, command).await;
    assert_eq!(result["success"], false, "{command}: {result}");
    let stderr = result["stderr"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("Read-only file system") || stderr.contains("read-only"),
        "{command}: {result}"
    );
}

#[tokio::test]
async fn protected_roots_are_read_only_and_the_config_dir_is_invisible_in_the_dev_container() {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let host = tempfile::tempdir().unwrap();
    let workspace = host.path().join("repo");
    let config_dir = host.path().join("config").join("nanna");
    std::fs::create_dir_all(workspace.join(".nanna/agents")).unwrap();
    std::fs::write(workspace.join(".nanna/agents/x.toml"), "[identity]").unwrap();
    std::fs::write(workspace.join("codecov.yml"), "coverage: {}").unwrap();
    std::fs::write(workspace.join("README.md"), "# repo").unwrap();
    std::fs::create_dir_all(config_dir.join("agents")).unwrap();
    std::fs::write(config_dir.join("agents/global.toml"), "[identity]").unwrap();

    let protected = ProtectedPaths::with_config_dir(&workspace, Some(&config_dir));
    let mounts: Vec<ReadOnlyMount> = protected
        .existing_roots(&workspace)
        .into_iter()
        .map(|root| {
            let container = Path::new(CONTAINER_WORKSPACE_DIR).join(&root);
            ReadOnlyMount::new(workspace.join(root), container)
        })
        .collect();
    assert_eq!(mounts.len(), 2, "{mounts:?}");
    assert!(mounts.iter().all(|m| !m.host.starts_with(&config_dir)));

    let Some(handle) = start(&workspace, mounts).await else {
        return;
    };
    let registry = create_container_tool_registry(&workspace, handle, CONTAINER_WORKSPACE_DIR);

    let config_path = config_dir.display().to_string();
    let visible = run(&registry, &format!("test -e '{config_path}'")).await;
    assert_eq!(visible["success"], false, "{visible}");
    let planted = run(
        &registry,
        &format!("mkdir -p '{config_path}' && echo x > '{config_path}/planted'"),
    )
    .await;
    assert_eq!(planted["success"], true, "{planted}");
    assert!(
        !config_dir.join("planted").exists(),
        "container write reached the host config dir"
    );
    assert_eq!(
        std::fs::read_to_string(config_dir.join("agents/global.toml")).unwrap(),
        "[identity]"
    );

    assert_refused(&registry, "echo tampered > /workspace/.nanna/agents/x.toml").await;
    assert_refused(&registry, "touch /workspace/.nanna/agents/new.toml").await;
    assert_refused(&registry, "rm /workspace/.nanna/agents/x.toml").await;
    assert_refused(&registry, "echo tampered > /workspace/codecov.yml").await;
    let removed = run(&registry, "rm -rf /workspace/.nanna").await;
    assert_eq!(removed["success"], false, "{removed}");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".nanna/agents/x.toml")).unwrap(),
        "[identity]"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("codecov.yml")).unwrap(),
        "coverage: {}"
    );
    assert!(!workspace.join(".nanna/agents/new.toml").exists());

    let ordinary = run(&registry, "echo edited > /workspace/README.md").await;
    assert_eq!(ordinary["success"], true, "{ordinary}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("README.md")).unwrap(),
        "edited\n"
    );
    assert_eq!(registry.denial_count(), 0);
}
