//! Container integration tests for the Postgres sidecar and the full-stack
//! dev container. All tests are `#[ignore]` and skip when no runtime is
//! available; run them with
//! `cargo test -p harness --test sidecar_integration -- --ignored --test-threads=1`.

use harness::container::{
    detect_runtime, exec_in_container, load_image_from_path, start_container_with_fallback,
    ContainerConfig, ContainerRuntime,
};
use harness::onboarding::{DeterministicOnboarder, Onboarder};
use harness::sidecar::{PostgresSidecar, ReadinessConfig, SidecarSet, SystemRunner};
use harness::workspace::TaskWorkspace;
use image_builder::build_dev_container;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const DEV_IMAGE_TAG: &str = "localhost/nanna-sidecar-test-dev:latest";

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git must be installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@test.invalid"]);
    git(dir, &["config", "user.name", "test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == "dist" || name == ".git" {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A stand-in dev container image: the Postgres image (which ships `psql`)
/// with an idle command, so the test does not depend on a Nix build.
fn build_psql_dev_image(runtime: &ContainerRuntime) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Containerfile"),
        "FROM docker.io/library/postgres:16\nCMD [\"sleep\", \"infinity\"]\n",
    )
    .unwrap();
    let out = Command::new(runtime.command())
        .args(["build", "-q", "-t", DEV_IMAGE_TAG])
        .arg(dir.path())
        .output()
        .expect("runtime must be runnable");
    assert!(
        out.status.success(),
        "image build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn network_exists(runtime: &ContainerRuntime, name: &str) -> bool {
    Command::new(runtime.command())
        .args(["network", "inspect", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Queries over TCP inside the sidecar, like the dev container would; the
/// unix socket is also served by the image's temporary init server.
fn current_database_in_sidecar(set: &SidecarSet, pg: &PostgresSidecar) -> String {
    let url = format!(
        "postgres://{}:{}@127.0.0.1:5432/{}",
        pg.user, pg.password, pg.database
    );
    let out = exec_in_container(
        &set.sidecars()[0].handle,
        &["psql", &url, "-tAc", "select current_database()"],
        None,
    )
    .unwrap();
    assert!(out.success, "psql in sidecar failed: {}", out.stderr);
    out.stdout.trim().to_string()
}

#[tokio::test]
#[ignore]
async fn postgres_sidecar_reachable_from_dev_container_with_injected_url() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_psql_dev_image(&runtime);

    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("README.md"), "sidecar test").unwrap();
    init_repo(source.path());

    let task_id = format!("sidecar-{}", uuid::Uuid::new_v4());
    let pg = PostgresSidecar::for_task(&task_id);
    let set = SidecarSet::start(
        runtime.clone(),
        Arc::new(SystemRunner),
        &task_id,
        &[pg.spec()],
        ReadinessConfig::default(),
    )
    .await
    .expect("postgres sidecar must start");
    let network = set.network_name().to_string();
    assert!(network_exists(&runtime, &network));

    let mut ws = TaskWorkspace::create_with_container_and_sidecars(
        source.path(),
        &task_id,
        "HEAD",
        DEV_IMAGE_TAG,
        Some(set),
    )
    .await
    .expect("dev container must start on the task network");
    assert_eq!(
        ws.sidecar_env(),
        &[("DATABASE_URL".to_string(), pg.database_url())]
    );

    let registry = ws.build_tool_registry();
    let result = registry
        .execute(
            "run_command",
            serde_json::json!({
                "command": "psql \"$DATABASE_URL\" -tAc 'select current_database()'"
            }),
        )
        .await
        .expect("run_command must execute");
    assert_eq!(
        result["success"], true,
        "psql via DATABASE_URL failed: {}",
        result["stderr"]
    );
    assert_eq!(result["stdout"].as_str().unwrap().trim(), pg.database);

    ws.cleanup().unwrap();
    assert!(
        !network_exists(&runtime, &network),
        "task network must be removed with the workspace"
    );
}

#[tokio::test]
#[ignore]
async fn two_tasks_get_distinct_databases() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    let ids = [
        format!("sidecar-a-{}", uuid::Uuid::new_v4()),
        format!("sidecar-b-{}", uuid::Uuid::new_v4()),
    ];
    let mut names = Vec::new();
    for task_id in &ids {
        let pg = PostgresSidecar::for_task(task_id);
        let set = SidecarSet::start(
            runtime.clone(),
            Arc::new(SystemRunner),
            task_id,
            &[pg.spec()],
            ReadinessConfig::default(),
        )
        .await
        .expect("postgres sidecar must start");
        let name = current_database_in_sidecar(&set, &pg);
        assert_eq!(name, pg.database);
        names.push(name);
    }
    assert_ne!(names[0], names[1]);
}

/// Onboards a copy of the fixture, builds its dev container from the
/// generated flake and checks that the profile's tools are on PATH and the
/// wasm target is installed.
#[tokio::test]
#[ignore]
async fn fixture_flake_builds_dev_container_with_profile_tools() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("fullstack");
    copy_dir_all(&fixture_root(), &repo);
    DeterministicOnboarder
        .onboard(&repo)
        .await
        .expect("fixture must onboard");
    let image_path = build_dev_container(&repo).expect("generated flake must build");
    let image_ref = load_image_from_path(&runtime, &image_path).expect("image must load");

    let config = ContainerConfig {
        base_image: image_ref,
        test_image: None,
        container_name: format!("nanna-fullstack-dev-test-{}", uuid::Uuid::new_v4()),
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(5),
        health_check_timeout: Duration::from_secs(5),
        env_vars: vec![],
        additional_args: vec![],
        network: harness::container::NetworkPolicy::Enabled,
        read_only_mounts: vec![],
    };
    let handle = start_container_with_fallback(&config)
        .await
        .expect("dev container must start");

    for argv in [
        ["trunk", "--version"],
        ["wasm-bindgen", "--version"],
        ["sqlx", "--version"],
        ["psql", "--version"],
    ] {
        let out = exec_in_container(&handle, &argv, None).unwrap();
        assert!(out.success, "{argv:?} failed: {}", out.stderr);
    }
    let targets = exec_in_container(
        &handle,
        &["sh", "-c", "ls \"$(rustc --print sysroot)/lib/rustlib\""],
        None,
    )
    .unwrap();
    assert!(
        targets.stdout.contains("wasm32-unknown-unknown"),
        "wasm target missing from toolchain: {}",
        targets.stdout
    );
}
