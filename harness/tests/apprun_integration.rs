//! Container integration tests for `app_start`, `app_logs` and `app_stop`
//! on the full-stack fixture. All tests are `#[ignore]` and skip when no
//! runtime is available; run them with
//! `cargo test -p harness --test apprun_integration -- --ignored --test-threads=1`.
//!
//! The dev image is a stand-in for the generated flake image: the official
//! Rust image plus the wasm target, a trunk release binary and the fixture's
//! dependencies pre-built into a fixed `CARGO_TARGET_DIR`, so the test only
//! pays for compiling the fixture crates themselves.

use harness::apprun::{Limits, DEFAULT_PORT_RANGE};
use harness::apprun::{PortAllocator, APP_LOGS_TOOL, APP_START_TOOL, APP_STOP_TOOL};
use harness::container::{detect_runtime, ContainerRuntime};
use harness::tools::ToolRegistry;
use harness::workspace::TaskWorkspace;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEV_IMAGE_TAG: &str = "localhost/nanna-apprun-test-dev:latest";
const TRUNK_URL: &str =
    "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-x86_64-unknown-linux-gnu.tar.gz";
const COLD_BUILD_LIMIT: Limits = Limits {
    max_wall_clock_secs: 1800,
};

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

fn build_dev_image(runtime: &ContainerRuntime) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), &dir.path().join("fixture"));
    let containerfile = format!(
        "FROM docker.io/library/rust:1-bookworm\n\
         RUN rustup target add wasm32-unknown-unknown \\\n\
         \x20&& curl -fsSL {TRUNK_URL} | tar -xz -C /usr/local/bin trunk \\\n\
         \x20&& mkdir -p /home/dev /cache \\\n\
         \x20&& apt-get update \\\n\
         \x20&& apt-get install -y --no-install-recommends chromium \\\n\
         \x20&& rm -rf /var/lib/apt/lists/*\n\
         ENV HOME=/home/dev CARGO_TARGET_DIR=/cache/target\n\
         COPY fixture /src\n\
         RUN cd /src/ui && trunk build && cd /src && cargo build --package api \\\n\
         \x20&& rm -rf /src && chmod -R a+w /cache /home/dev /usr/local/cargo\n\
         CMD [\"sleep\", \"infinity\"]\n"
    );
    std::fs::write(dir.path().join("Containerfile"), containerfile).unwrap();
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

fn container_exists(runtime: &ContainerRuntime, name: &str) -> bool {
    Command::new(runtime.command())
        .args(["container", "exists", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn curl(registry: &ToolRegistry, url: &str) -> Value {
    registry
        .execute(
            "run_command",
            json!({ "command": format!("curl -sf {url}") }),
        )
        .await
        .expect("run_command must execute")
}

async fn app(registry: &ToolRegistry, tool: &str, args: Value) -> Value {
    registry
        .execute(tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

#[tokio::test]
#[ignore]
async fn app_start_serves_index_and_api_until_app_stop_and_cleanup() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&runtime);

    let source = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), source.path());
    init_repo(source.path());
    let task_id = format!("apprun-{}", uuid::Uuid::new_v4());
    let container_name = format!("nanna-task-{task_id}");

    let mut ws =
        TaskWorkspace::create_with_container(source.path(), &task_id, "HEAD", DEV_IMAGE_TAG)
            .await
            .expect("dev container must start");
    ws.set_app_limits(Some(COLD_BUILD_LIMIT));
    let registry = ws.build_tool_registry();
    for name in [APP_START_TOOL, APP_STOP_TOOL, APP_LOGS_TOOL] {
        assert!(
            registry.get_tool(name).is_some(),
            "{name} must be registered"
        );
    }

    let started = app(&registry, APP_START_TOOL, Value::Null).await;
    let base_url = started["base_url"].as_str().unwrap().to_string();
    let port = u16::try_from(started["port"].as_u64().unwrap()).unwrap();
    assert_eq!(base_url, format!("http://127.0.0.1:{port}"));
    assert!(
        DEFAULT_PORT_RANGE.contains(&port),
        "port {port} outside the default range"
    );
    assert_eq!(started["api_url"], base_url);
    assert_eq!(started["frontend_url"], base_url);
    assert_eq!(started["task_id"], task_id);
    assert_eq!(started["log_path"], format!("/tmp/nanna-app-{task_id}.log"));
    assert_eq!(PortAllocator::shared().held(), vec![port]);

    let health = curl(&registry, &format!("{base_url}/health/v1")).await;
    assert_eq!(health["success"], true, "{health}");
    assert!(health["stdout"].as_str().unwrap().contains("\"ok\""));
    let index = curl(
        &registry,
        &format!("{}/", started["frontend_url"].as_str().unwrap()),
    )
    .await;
    assert_eq!(index["success"], true, "{index}");
    let html = index["stdout"].as_str().unwrap();
    assert!(html.contains("<html"), "index page: {html}");
    assert!(html.contains("Full-stack fixture"), "index page: {html}");
    assert!(
        html.contains(".wasm"),
        "index page must load the wasm bundle: {html}"
    );
    let greeting = curl(
        &registry,
        &format!("{}/api/v1/greeting", started["api_url"].as_str().unwrap()),
    )
    .await;
    assert_eq!(greeting["success"], true, "{greeting}");
    assert!(greeting["stdout"]
        .as_str()
        .unwrap()
        .contains("Hello from the full-stack fixture"));

    let logs = app(&registry, APP_LOGS_TOOL, json!({ "tail": 50 })).await;
    assert_eq!(logs["log_path"], started["log_path"]);
    let lines = logs["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().unwrap().contains("starting fixture api")),
        "log lines: {lines:?}"
    );

    let again = app(&registry, APP_START_TOOL, Value::Null).await;
    assert_eq!(
        again, started,
        "second app_start returns the running instance"
    );

    let stopped = app(&registry, APP_STOP_TOOL, Value::Null).await;
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert_eq!(stopped["killed"], true, "{stopped}");
    assert_eq!(stopped["pid"], started["pid"]);
    assert_eq!(stopped["port"], started["port"]);
    assert!(PortAllocator::shared().held().is_empty());
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let after_stop = curl(&registry, &format!("{base_url}/health/v1")).await;
    assert_eq!(
        after_stop["success"], false,
        "the app must be gone: {after_stop}"
    );
    let stopped_again = app(&registry, APP_STOP_TOOL, Value::Null).await;
    assert_eq!(
        stopped_again,
        json!({ "task_id": task_id, "stopped": false })
    );

    let restarted = app(&registry, APP_START_TOOL, Value::Null).await;
    assert_ne!(
        restarted["pid"], started["pid"],
        "restart starts a new process"
    );
    let health = curl(
        &registry,
        &format!("{}/health/v1", restarted["base_url"].as_str().unwrap()),
    )
    .await;
    assert_eq!(health["success"], true, "{health}");
    let restarted_port = u16::try_from(restarted["port"].as_u64().unwrap()).unwrap();

    ws.cleanup()
        .expect("cleanup must succeed with a running app");
    assert!(
        PortAllocator::shared().held().is_empty(),
        "cleanup releases the port"
    );
    assert!(!PortAllocator::shared().lease_path(restarted_port).exists());
    assert!(
        !container_exists(&runtime, &container_name),
        "dev container must be removed"
    );
}
