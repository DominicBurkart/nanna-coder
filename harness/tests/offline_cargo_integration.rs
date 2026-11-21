//! Container integration test for offline cargo dependencies.
//!
//! A workspace-ceiling task runs with `--network=none`. The host vendors the
//! repository's registry dependencies, mounts the sources and a generated
//! source-replacement config read-only, and `cargo run --offline` then builds
//! a crate that depends on a vendored dependency inside the container. It
//! also checks that the mounts cannot be written from the container, that
//! the network is really off, and that a repository declaring its own cargo
//! source replacement is refused before anything starts.
//!
//! The dependency is a throwaway git repository created in a temporary
//! directory, so no test here needs network access (the Nix build sandbox has
//! none). The container test skips (loudly) when no container runtime is
//! available and needs an image with cargo: `NANNA_OFFLINE_CARGO_IMAGE`
//! (default `docker.io/library/rust:1-bookworm`).

mod support;

use harness::container::{
    detect_runtime, start_container_with_fallback, ContainerConfig, ContainerHandle, NetworkPolicy,
};
use harness::offline_cargo::{CargoVendor, OfflineCargo, OfflineCargoError};
use harness::tools::{create_container_tool_registry, ToolRegistry, CONTAINER_WORKSPACE_DIR};
use harness::workspace::worktree_mount_arg;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_IMAGE: &str = "docker.io/library/rust:1-bookworm";

fn image() -> String {
    std::env::var("NANNA_OFFLINE_CARGO_IMAGE").unwrap_or_else(|_| DEFAULT_IMAGE.to_string())
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.com",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

fn write_crate(root: &Path, dep: &Path) {
    std::fs::create_dir_all(dep.join("src")).unwrap();
    std::fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"localdep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        dep.join("src/lib.rs"),
        "pub fn seven() -> &'static str {\n    \"7\"\n}\n",
    )
    .unwrap();
    git(dep, &["init", "-q"]);
    git(dep, &["add", "."]);
    git(dep, &["commit", "-q", "-m", "dependency"]);

    std::fs::create_dir_all(root.join("src")).unwrap();
    let manifest = format!(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nlocaldep = {{ git = \"file://{}\" }}\n",
        dep.display()
    );
    std::fs::write(root.join("Cargo.toml"), manifest).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"sum={}\", localdep::seven()); }\n",
    )
    .unwrap();
    let locked = Command::new("cargo")
        .args(["generate-lockfile"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(locked.status.success(), "cargo generate-lockfile failed");
}

async fn start(workspace: &Path, offline: &OfflineCargo) -> Arc<ContainerHandle> {
    let config = ContainerConfig {
        base_image: image(),
        test_image: None,
        container_name: format!("nanna-offline-{}", uuid::Uuid::new_v4()),
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(5),
        health_check_timeout: Duration::from_secs(5),
        env_vars: {
            let mut env = vec![("CARGO_HOME".to_string(), "/tmp/cargo".to_string())];
            env.extend(offline.env().iter().cloned());
            env
        },
        additional_args: vec![
            "-i".to_string(),
            "-t".to_string(),
            worktree_mount_arg(workspace),
            "--tmpfs=/tmp:rw,mode=1777".to_string(),
        ],
        network: NetworkPolicy::Disabled,
        read_only_mounts: offline.mounts().to_vec(),
    };
    Arc::new(
        start_container_with_fallback(&config)
            .await
            .expect("container start failed; the cargo image must be available"),
    )
}

async fn run(registry: &ToolRegistry, command: &str) -> Value {
    registry
        .execute("run_command", json!({ "command": command }))
        .await
        .expect("run_command should execute")
}

#[tokio::test]
async fn a_network_less_container_builds_a_dependency_from_vendored_sources() {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let host = tempfile::tempdir().unwrap();
    let workspace = host.path().join("repo");
    write_crate(&workspace, &host.path().join("dep"));
    let stage = host.path().join("stage");

    let offline = OfflineCargo::prepare(&workspace, &stage, &CargoVendor)
        .expect("vendoring should succeed")
        .expect("the crate has a vendored dependency");

    let handle = start(&workspace, &offline).await;
    let registry = support::allow_gated(create_container_tool_registry(
        &workspace,
        handle,
        CONTAINER_WORKSPACE_DIR,
    ));

    let built = run(&registry, "cargo run --offline --quiet").await;
    assert_eq!(built["success"], true, "{built}");
    let printed = built["stdout"].as_str().unwrap_or_default();
    assert!(printed.contains("sum=7"), "{built}");

    let vendor_write = run(&registry, "touch /opt/nanna-offline/vendor/planted").await;
    assert_eq!(vendor_write["success"], false, "{vendor_write}");
    let config_write = run(&registry, "echo x > /.cargo/config.toml").await;
    assert_eq!(config_write["success"], false, "{config_write}");

    let network = run(
        &registry,
        "getent hosts crates.io || echo no-network-resolution",
    )
    .await;
    let resolved = network["stdout"].as_str().unwrap_or_default();
    assert!(resolved.contains("no-network-resolution"), "{network}");

    offline.cleanup();
    assert!(!stage.exists());
}

#[test]
fn a_dependency_is_vendored_on_the_host_into_read_only_container_mounts() {
    let host = tempfile::tempdir().unwrap();
    let workspace = host.path().join("repo");
    write_crate(&workspace, &host.path().join("dep"));
    let stage = host.path().join("stage");

    let offline = OfflineCargo::prepare(&workspace, &stage, &CargoVendor)
        .expect("vendoring should succeed")
        .expect("the crate has a vendored dependency");

    assert!(stage.join("vendor/localdep").is_dir());
    let config = std::fs::read_to_string(stage.join("config.toml")).unwrap();
    assert!(config.contains("directory = \"/opt/nanna-offline/vendor\""));
    assert!(config.contains("offline = true"));
    let args: Vec<String> = offline.mounts().iter().map(|m| m.run_arg()).collect();
    assert_eq!(args.len(), 2);
    assert!(args.iter().all(|arg| arg.ends_with(":ro,z")));
    let expected_env = vec![("CARGO_NET_OFFLINE".to_string(), "true".to_string())];
    assert_eq!(offline.env(), expected_env.as_slice());

    offline.cleanup();
    assert!(!stage.exists());
}

#[test]
fn a_repo_declaring_its_own_cargo_source_is_refused_before_vendoring() {
    let host = tempfile::tempdir().unwrap();
    let workspace = host.path().join("repo");
    std::fs::create_dir_all(workspace.join(".cargo")).unwrap();
    std::fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(
        workspace.join(".cargo/config.toml"),
        "[source.crates-io]\nreplace-with = \"mirror\"\n[source.mirror]\ndirectory = \"/x\"\n",
    )
    .unwrap();
    let stage = host.path().join("stage");

    let result = OfflineCargo::prepare(&workspace, &stage, &CargoVendor);

    let refused = matches!(result, Err(OfflineCargoError::RepoOverride { .. }));
    assert!(refused);
    assert!(!stage.exists());
}
