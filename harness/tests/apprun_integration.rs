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
use harness::sidecar::{
    build_image_from_containerfile, container_exists, CommandRunner, RunOutput, SidecarError,
    SystemRunner,
};
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

fn dev_containerfile() -> String {
    format!(
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
    )
}

fn build_dev_image(
    runner: &dyn CommandRunner,
    runtime: &ContainerRuntime,
) -> Result<(), SidecarError> {
    let dir = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), &dir.path().join("fixture"));
    build_image_from_containerfile(
        runner,
        runtime,
        DEV_IMAGE_TAG,
        dir.path(),
        &dev_containerfile(),
    )
}

#[tokio::test]
#[ignore]
async fn app_start_serves_index_and_api_until_app_stop_and_cleanup() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&SystemRunner, &runtime).unwrap();

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
        !container_exists(&SystemRunner, &runtime, &container_name),
        "dev container must be removed"
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

struct StubTool {
    name: &'static str,
    reply: Value,
    seen: std::sync::Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl harness::tools::Tool for StubTool {
    fn definition(&self) -> model::types::ToolDefinition {
        model::types::ToolDefinition {
            function: model::types::FunctionDefinition {
                name: self.name.to_string(),
                description: "stub".to_string(),
                parameters: model::types::JsonSchema {
                    schema_type: model::types::SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> harness::tools::ToolResult<Value> {
        self.seen.lock().unwrap().push(args);
        Ok(self.reply.clone())
    }

    fn name(&self) -> &str {
        self.name
    }

    fn effect_class(&self) -> harness::effects::EffectClass {
        harness::effects::EffectClass::Workspace
    }
}

fn registry_with(name: &'static str, reply: Value) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(StubTool {
        name,
        reply,
        seen: std::sync::Mutex::new(vec![]),
    }));
    registry
}

#[tokio::test]
async fn curl_runs_a_silent_failing_curl_through_run_command() {
    let registry = registry_with("run_command", json!({ "success": true, "stdout": "ok" }));
    let out = curl(&registry, "http://127.0.0.1:1/x").await;
    assert_eq!(out["stdout"], "ok");
}

#[tokio::test]
#[should_panic(expected = "run_command must execute")]
async fn curl_panics_when_run_command_is_missing() {
    curl(&ToolRegistry::new(), "http://127.0.0.1:1/x").await;
}

#[tokio::test]
async fn app_returns_the_tool_result() {
    let registry = registry_with("app_start", json!({ "port": 1 }));
    assert_eq!(app(&registry, "app_start", Value::Null).await["port"], 1);
}

#[tokio::test]
#[should_panic(expected = "app_stop failed")]
async fn app_panics_naming_the_failing_tool() {
    app(&ToolRegistry::new(), "app_stop", Value::Null).await;
}

#[test]
fn stub_tool_describes_itself_by_name() {
    let registry = registry_with("probe", Value::Null);
    let tool = registry.get_tool("probe").unwrap();
    assert_eq!(tool.definition().function.name, "probe");
    assert_eq!(tool.definition().function.description, "stub");
}

#[test]
fn dev_containerfile_installs_the_toolchain_and_prebuilds_the_fixture() {
    let containerfile = dev_containerfile();
    assert!(containerfile.starts_with("FROM docker.io/library/rust:1-bookworm\n"));
    assert!(containerfile.contains("rustup target add wasm32-unknown-unknown"));
    assert!(containerfile.contains(TRUNK_URL));
    assert!(containerfile.contains("COPY fixture /src"));
    assert!(containerfile.contains("trunk build"));
    assert!(containerfile.contains("cargo build --package api"));
    assert!(containerfile.ends_with("CMD [\"sleep\", \"infinity\"]\n"));
}

type BuildCall = (String, Vec<String>, String, bool);

struct RecordingBuildRunner {
    success: bool,
    seen: std::sync::Mutex<Vec<BuildCall>>,
}

impl CommandRunner for RecordingBuildRunner {
    fn run(&self, program: &str, args: &[String]) -> std::io::Result<RunOutput> {
        let context = Path::new(args.last().unwrap());
        let containerfile = std::fs::read_to_string(context.join("Containerfile")).unwrap();
        let fixture_copied = context.join("fixture/Cargo.toml").is_file();
        self.seen.lock().unwrap().push((
            program.to_string(),
            args.to_vec(),
            containerfile,
            fixture_copied,
        ));
        Ok(RunOutput {
            success: self.success,
            stdout: String::new(),
            stderr: "boom".to_string(),
        })
    }
}

#[test]
fn build_dev_image_builds_the_tagged_image_from_the_copied_fixture() {
    let runner = RecordingBuildRunner {
        success: true,
        seen: std::sync::Mutex::new(vec![]),
    };
    build_dev_image(&runner, &ContainerRuntime::Podman).unwrap();
    let seen = runner.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (program, args, containerfile, fixture_copied) = &seen[0];
    assert_eq!(program, "podman");
    assert_eq!(&args[..3], ["build", "-q", "-t"]);
    assert_eq!(args[3], DEV_IMAGE_TAG);
    assert_eq!(containerfile, &dev_containerfile());
    assert!(fixture_copied, "fixture must be copied into the context");
}

#[test]
fn build_dev_image_surfaces_a_failed_build() {
    let runner = RecordingBuildRunner {
        success: false,
        seen: std::sync::Mutex::new(vec![]),
    };
    let err = build_dev_image(&runner, &ContainerRuntime::Podman).unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
}
