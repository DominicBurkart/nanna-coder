//! Container integration tests for the Postgres sidecar and the full-stack
//! dev container. All tests are `#[ignore]` and skip when no runtime is
//! available; run them with
//! `cargo test -p harness --test sidecar_integration -- --ignored --test-threads=1`.

use harness::container::NetworkPolicy;
use harness::container::{
    detect_runtime, exec_in_container, load_image_from_path, start_container_with_fallback,
    ContainerConfig,
};
use harness::onboarding::{DeterministicOnboarder, Onboarder};
use harness::sidecar::{
    build_image_from_containerfile, network_exists, task_leftovers, PostgresSidecar,
    ReadinessConfig, SidecarSet, SystemRunner,
};
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

#[tokio::test]
#[ignore]
async fn postgres_sidecar_reachable_from_dev_container_with_injected_url() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    let image_context = tempfile::tempdir().unwrap();
    build_image_from_containerfile(
        &SystemRunner,
        &runtime,
        DEV_IMAGE_TAG,
        image_context.path(),
        "FROM docker.io/library/postgres:16\nCMD [\"sleep\", \"infinity\"]\n",
    )
    .unwrap();

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
    assert!(network_exists(&SystemRunner, &runtime, &network));

    let mut ws = TaskWorkspace::create_with_container_and_sidecars(
        source.path(),
        &task_id,
        "HEAD",
        DEV_IMAGE_TAG,
        NetworkPolicy::Enabled,
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
        !network_exists(&SystemRunner, &runtime, &network),
        "task network must be removed with the workspace"
    );
    assert_eq!(
        task_leftovers(&SystemRunner, &runtime, &task_id),
        Vec::<String>::new(),
        "no nanna-task resource may outlive the workspace"
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
        let probe = pg.current_database_probe();
        let probe: Vec<&str> = probe.iter().map(String::as_str).collect();
        let out = exec_in_container(&set.sidecars()[0].handle, &probe, None).unwrap();
        assert!(out.success, "psql in sidecar failed: {}", out.stderr);
        let name = out.stdout.trim().to_string();
        assert_eq!(name, pg.database);
        names.push(name);
        drop(set);
        assert_eq!(
            task_leftovers(&SystemRunner, &runtime, task_id),
            Vec::<String>::new(),
            "dropping the sidecar set must remove its containers and network"
        );
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
        network: NetworkPolicy::Enabled,
        read_only_mounts: harness::container::NO_READ_ONLY_MOUNTS,
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

#[test]
fn fixture_root_is_the_fullstack_fixture() {
    let root = fixture_root();
    assert!(root.join("Cargo.toml").is_file(), "{}", root.display());
}

#[test]
fn init_repo_commits_every_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/b.txt"), "b").unwrap();
    init_repo(dir.path());
    let out = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .current_dir(dir.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success());
    let tracked: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(tracked, vec!["a.txt", "sub/b.txt"]);
}

#[test]
#[should_panic(expected = "failed")]
fn git_helper_panics_on_a_failing_command() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["rev-parse", "HEAD"]);
}

#[test]
fn copy_dir_all_copies_sources_and_skips_build_and_vcs_directories() {
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("api/src")).unwrap();
    std::fs::write(src.path().join("api/src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(src.path().join("Cargo.toml"), "[workspace]").unwrap();
    for skipped in ["target", "dist", ".git"] {
        std::fs::create_dir_all(src.path().join(skipped)).unwrap();
        std::fs::write(src.path().join(skipped).join("junk"), "x").unwrap();
    }
    let dst = tempfile::tempdir().unwrap();
    let out = dst.path().join("copy");
    copy_dir_all(src.path(), &out);
    assert_eq!(
        std::fs::read_to_string(out.join("api/src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert!(out.join("Cargo.toml").is_file());
    for skipped in ["target", "dist", ".git"] {
        assert!(!out.join(skipped).exists(), "{skipped} must not be copied");
    }
}
