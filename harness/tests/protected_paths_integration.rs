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
//! It skips (loudly) when no container runtime is available and fails when
//! the runtime cannot start the image.

mod support;

use harness::container::{
    detect_runtime, start_container_with_fallback, ContainerConfig, ContainerHandle, NetworkPolicy,
    ReadOnlyMount,
};
use harness::protected::{ProtectedExtras, ProtectedPaths};
use harness::tools::{create_container_tool_registry, ToolRegistry, CONTAINER_WORKSPACE_DIR};
use harness::workspace::protected_mounts;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const IMAGE: &str = "docker.io/library/alpine:latest";

async fn start(workspace: &Path, mounts: Vec<ReadOnlyMount>) -> Arc<ContainerHandle> {
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
    let handle = start_container_with_fallback(&config)
        .await
        .expect("container start failed; the alpine image must be available");
    Arc::new(handle)
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
    let read_only = stderr.contains("Read-only file system") || stderr.contains("read-only");
    assert!(read_only, "{command}: {result}");
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

    let handle = start(&workspace, mounts).await;
    let registry = support::allow_gated(create_container_tool_registry(
        &workspace,
        handle,
        CONTAINER_WORKSPACE_DIR,
    ));

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

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

async fn assert_blocked(registry: &ToolRegistry, command: &str) {
    let result = run(registry, command).await;
    assert_eq!(result["success"], false, "{command}: {result}");
}

#[tokio::test]
async fn protected_roots_absent_at_container_start_cannot_be_created_from_the_shell() {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let host = tempfile::tempdir().unwrap();
    let source = host.path().join("source");
    let workspace = host.path().join("worktree");
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.email", "t@t.com"]);
    git(&source, &["config", "user.name", "T"]);
    git(&source, &["config", "commit.gpgsign", "false"]);
    std::fs::write(source.join("README.md"), "# repo").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "init"]);
    git(
        &source,
        &["worktree", "add", "-q", workspace.to_str().unwrap(), "HEAD"],
    );
    for absent in [".nanna", ".github", "codecov.yml", "windows.toml"] {
        assert!(!workspace.join(absent).exists(), "{absent}");
    }
    let gitlink = std::fs::read_to_string(workspace.join(".git")).unwrap();
    assert!(gitlink.starts_with("gitdir:"), "{gitlink}");

    let extras = ProtectedExtras::parse("extra = [\"src/gate.rs\", \"tools/**\"]").unwrap();
    let protected = ProtectedPaths::with_extras(&workspace, None, &extras);
    let shielded = protected_mounts(&protected, &workspace).unwrap();

    let handle = start(&workspace, shielded.mounts).await;
    let registry = support::allow_gated(create_container_tool_registry(
        &workspace,
        handle,
        CONTAINER_WORKSPACE_DIR,
    ));

    assert_refused(&registry, "mkdir -p /workspace/.nanna/agents").await;
    assert_refused(&registry, "echo x > /workspace/.nanna/new.toml").await;
    assert_refused(&registry, "mkdir -p /workspace/.github/workflows/sub").await;
    assert_refused(&registry, "echo x > /workspace/.github/dependabot.yml").await;
    assert_refused(&registry, "mkdir -p /workspace/.github/actions/setup").await;
    assert_refused(&registry, "echo x > /workspace/.envrc").await;
    assert_refused(&registry, "echo x > /workspace/CLAUDE.md").await;
    assert_refused(&registry, "mkdir -p /workspace/nix/sub").await;
    assert_refused(&registry, "echo x > /workspace/codecov.yml").await;
    assert_refused(&registry, "echo x > /workspace/windows.toml").await;
    assert_refused(&registry, "echo x > /workspace/src/gate.rs").await;
    assert_refused(&registry, "echo x > /workspace/tools/new.sh").await;
    assert_refused(&registry, "mkdir /workspace/tools/sub").await;
    assert_blocked(&registry, "echo 'gitdir: /tmp/evil' > /workspace/.git").await;
    assert_blocked(&registry, "rm -f /workspace/.git").await;
    assert_blocked(&registry, "mkdir -p /workspace/.git/hooks").await;
    assert_blocked(&registry, "echo x > /workspace/.git/hooks/pre-commit").await;
    assert_blocked(&registry, "echo x > /workspace/.git/config").await;

    assert_eq!(
        std::fs::read_to_string(workspace.join(".git")).unwrap(),
        gitlink
    );
    assert_eq!(
        std::fs::read_dir(workspace.join(".nanna")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(workspace.join(".github"))
            .unwrap()
            .count(),
        0
    );
    for placeholder in ["codecov.yml", "windows.toml", ".envrc", "CLAUDE.md"] {
        assert_eq!(
            std::fs::metadata(workspace.join(placeholder))
                .unwrap()
                .len(),
            0
        );
    }
    git(&workspace, &["status", "--porcelain"]);

    let ordinary = run(&registry, "echo edited > /workspace/README.md").await;
    assert_eq!(ordinary["success"], true, "{ordinary}");
    assert_eq!(registry.denial_count(), 0);
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn moving_the_parent_of_a_declared_file_and_recreating_it_never_changes_a_declared_path_unseen(
) {
    if !detect_runtime().is_available() {
        eprintln!("SKIPPED: no container runtime available");
        return;
    }
    let host = tempfile::tempdir().unwrap();
    let source = host.path().join("source");
    let workspace = host.path().join("worktree");
    std::fs::create_dir_all(source.join("src")).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.email", "t@t.com"]);
    git(&source, &["config", "user.name", "T"]);
    git(&source, &["config", "commit.gpgsign", "false"]);
    std::fs::write(source.join("src/gate.rs"), "original\n").unwrap();
    std::fs::write(source.join("src/other.rs"), "other\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "init"]);
    git(
        &source,
        &["worktree", "add", "-q", workspace.to_str().unwrap(), "HEAD"],
    );

    let extras = ProtectedExtras::parse("extra = [\"src/gate.rs\"]").unwrap();
    let protected = ProtectedPaths::with_extras(&workspace, None, &extras);
    let shielded = protected_mounts(&protected, &workspace).unwrap();
    let handle = start(&workspace, shielded.mounts).await;
    let registry = support::allow_gated(create_container_tool_registry(
        &workspace,
        handle,
        CONTAINER_WORKSPACE_DIR,
    ));

    let moved = run(&registry, "mv /workspace/src /workspace/moved").await;
    let recreated = run(
        &registry,
        "mkdir -p /workspace/src && echo evil > /workspace/src/gate.rs",
    )
    .await;
    let overwritten = run(&registry, "echo evil > /workspace/src/gate.rs").await;
    eprintln!("mv parent: {moved}\nrecreate: {recreated}\noverwrite: {overwritten}");

    let status = git_out(
        &workspace,
        &["status", "--porcelain", "--untracked-files=all", "-z"],
    );
    let changed: Vec<&str> = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .map(|entry| &entry[3..])
        .collect();
    let gate_on_host = std::fs::read_to_string(workspace.join("src/gate.rs")).ok();
    if gate_on_host.as_deref() != Some("original\n") {
        assert!(
            changed.contains(&"src/gate.rs"),
            "declared file changed but git does not report it: {changed:?}"
        );
        let violation = protected.check(Path::new("src/gate.rs")).unwrap_err();
        assert_eq!(violation.rule, "src/gate.rs");
    }
    if moved["success"] == true {
        assert!(
            changed.contains(&"src/gate.rs"),
            "parent moved but the declared path is not reported: {changed:?}"
        );
    }
    assert!(
        std::fs::read_to_string(workspace.join("src/other.rs")).is_ok() || moved["success"] == true
    );
}
