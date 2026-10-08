//! Container integration tests for `qa_endpoints` and `qa_browser` on the
//! full-stack fixture. All tests are `#[ignore]` and skip when no runtime is
//! available; run them with
//! `cargo test -p harness --test qa_integration -- --ignored --test-threads=1`.
//!
//! The dev image is the [`apprun_integration`] stand-in plus headless
//! Chromium (the package the generated flake adds for this profile).
//! `qa_browser`'s console-error evidence comes from Chromium's own
//! automatic `GET /favicon.ico` request, which the fixture's static file
//! service 404s (no file exists under `ui/dist`); no fixture change was
//! needed to produce it.

use harness::apprun::{Limits, APP_START_TOOL};
use harness::container::{detect_runtime, ContainerRuntime};
use harness::qa::{QA_BROWSER_TOOL, QA_ENDPOINTS_TOOL};
use harness::sidecar::{
    build_image_from_containerfile, CommandRunner, RunOutput, SidecarError, SystemRunner,
};
use harness::tools::ToolRegistry;
use harness::workspace::TaskWorkspace;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEV_IMAGE_TAG: &str = "localhost/nanna-qa-test-dev:latest";
const TRUNK_URL: &str =
    "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-x86_64-unknown-linux-gnu.tar.gz";
const DEV_CONTAINERFILE: &str = "FROM docker.io/library/rust:1-bookworm\n\
         RUN rustup target add wasm32-unknown-unknown \\\n\
         \x20&& curl -fsSL @TRUNK_URL@ | tar -xz -C /usr/local/bin trunk \\\n\
         \x20&& mkdir -p /home/dev /cache \\\n\
         \x20&& apt-get update \\\n\
         \x20&& apt-get install -y --no-install-recommends chromium \\\n\
         \x20&& rm -rf /var/lib/apt/lists/*\n\
         ENV HOME=/home/dev CARGO_TARGET_DIR=/cache/target\n\
         COPY fixture /src\n\
         RUN cd /src/ui && trunk build && cd /src && cargo build --package api \\\n\
         \x20&& rm -rf /src && chmod -R a+w /cache /home/dev /usr/local/cargo\n\
         CMD [\"sleep\", \"infinity\"]\n";
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

async fn tool(registry: &ToolRegistry, name: &str, args: Value) -> Value {
    registry
        .execute(name, args)
        .await
        .unwrap_or_else(|e| panic!("{name} failed: {e}"))
}

async fn start_workspace(
    source: &Path,
    task_prefix: &str,
    env: Vec<(String, String)>,
) -> TaskWorkspace {
    let task_id = format!("{task_prefix}-{}", uuid::Uuid::new_v4());
    let mut ws = TaskWorkspace::create_with_container(source, &task_id, "HEAD", DEV_IMAGE_TAG)
        .await
        .expect("dev container must start");
    ws.set_app_limits(Some(COLD_BUILD_LIMIT));
    if !env.is_empty() {
        ws.set_app_env(env);
    }
    ws
}

fn dev_containerfile() -> String {
    DEV_CONTAINERFILE.replace("@TRUNK_URL@", TRUNK_URL)
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
async fn qa_tools_reject_calls_before_app_start_then_check_and_mount_the_fixture() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&SystemRunner, &runtime).unwrap();

    let source = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), source.path());
    init_repo(source.path());
    let mut ws = start_workspace(source.path(), "qa-precheck", vec![]).await;
    let registry = ws.build_tool_registry();

    for (name, args) in [
        (QA_ENDPOINTS_TOOL, json!({})),
        (
            QA_BROWSER_TOOL,
            json!({ "scenario": { "steps": [{ "step": "goto", "path": "/" }] } }),
        ),
    ] {
        let err = registry.execute(name, args).await.unwrap_err();
        assert!(
            err.to_string().contains("no application is running"),
            "{name}: {err}"
        );
    }

    tool(&registry, APP_START_TOOL, Value::Null).await;

    let endpoints = tool(&registry, QA_ENDPOINTS_TOOL, json!({})).await;
    assert_eq!(endpoints["manifest"], "CHECKS", "{endpoints}");
    assert_eq!(endpoints["all_passed"], true, "{endpoints}");
    assert_eq!(endpoints["passed"], 3, "{endpoints}");
    let paths: Vec<&str> = endpoints["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/", "/health/v1", "/api/v1/greeting"]);
    assert!(
        Path::new(endpoints["artifact"].as_str().unwrap()).is_file(),
        "{endpoints}"
    );

    let scenario = json!({ "steps": [
        { "step": "goto", "path": "/" },
        { "step": "expect_text", "selector": "#greeting", "text": "Hello from the full-stack fixture" },
        { "step": "screenshot", "name": "home" }
    ] });
    let browser = tool(&registry, QA_BROWSER_TOOL, json!({ "scenario": scenario })).await;
    assert_eq!(browser["passed"], true, "{browser}");
    let screenshots = browser["screenshots"].as_array().unwrap();
    assert_eq!(screenshots.len(), 1, "{browser}");
    let screenshot = PathBuf::from(screenshots[0].as_str().unwrap());
    assert!(screenshot.is_file(), "{screenshot:?}");
    assert!(
        std::fs::metadata(&screenshot).unwrap().len() > 0,
        "screenshot must not be empty"
    );
    let console_errors = browser["console_errors"].as_array().unwrap();
    assert!(
        console_errors.iter().any(|e| e["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/favicon.ico"))
            && e["text"].as_str().is_some_and(|t| t.contains("404"))),
        "the browser's automatic favicon.ico request must 404 and be reported: {browser}"
    );

    let summary = ws.qa_summary();
    assert_eq!(summary.endpoint_runs, 1);
    assert_eq!(summary.browser_runs, 1);
    assert!(!summary.artifacts.is_empty());

    ws.cleanup().expect("cleanup must succeed");
}

#[tokio::test]
#[ignore]
async fn qa_endpoints_reports_the_broken_route_with_its_response_snippet() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&SystemRunner, &runtime).unwrap();

    let source = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), source.path());
    init_repo(source.path());
    let mut ws = start_workspace(
        source.path(),
        "qa-break",
        vec![("FIXTURE_BREAK_ROUTE".to_string(), "1".to_string())],
    )
    .await;
    let registry = ws.build_tool_registry();

    tool(&registry, APP_START_TOOL, Value::Null).await;
    let endpoints = tool(&registry, QA_ENDPOINTS_TOOL, json!({})).await;
    assert_eq!(endpoints["all_passed"], false, "{endpoints}");
    assert_eq!(endpoints["passed"], 2, "{endpoints}");
    assert_eq!(endpoints["failed"], 1, "{endpoints}");
    let checks = endpoints["checks"].as_array().unwrap();
    let health = checks.iter().find(|c| c["path"] == "/health/v1").unwrap();
    assert_eq!(health["passed"], true, "{health}");
    let greeting = checks
        .iter()
        .find(|c| c["path"] == "/api/v1/greeting")
        .unwrap();
    assert_eq!(greeting["passed"], false, "{greeting}");
    assert_eq!(greeting["status"], 500, "{greeting}");
    assert!(
        greeting["snippet"]
            .as_str()
            .unwrap()
            .contains("route broken by FIXTURE_BREAK_ROUTE=1"),
        "{greeting}"
    );

    ws.cleanup().expect("cleanup must succeed");
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
async fn tool_returns_the_tool_result() {
    let registry = registry_with("qa_endpoints", json!({ "passed": 2 }));
    assert_eq!(
        tool(&registry, "qa_endpoints", json!({})).await["passed"],
        2
    );
}

#[tokio::test]
#[should_panic(expected = "qa_browser failed")]
async fn tool_panics_naming_the_failing_tool() {
    tool(&ToolRegistry::new(), "qa_browser", json!({})).await;
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

#[cfg(unix)]
#[tokio::test]
async fn start_workspace_applies_the_cold_build_limit_and_app_env() {
    use std::os::unix::fs::PermissionsExt;

    let bin_dir = tempfile::tempdir().unwrap();
    let podman = bin_dir.path().join("podman");
    std::fs::write(&podman, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&podman, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&old_path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("a.txt"), "a").unwrap();
    init_repo(source.path());
    let (mut plain, mut with_env) = tokio::join!(
        start_workspace(source.path(), "qa-plain", vec![]),
        start_workspace(
            source.path(),
            "qa-env",
            vec![("K".to_string(), "V".to_string())],
        ),
    );
    std::env::set_var("PATH", old_path);

    assert_eq!(plain.app_limits(), COLD_BUILD_LIMIT);
    assert!(plain.app_env().is_empty());
    assert_eq!(with_env.app_limits(), COLD_BUILD_LIMIT);
    assert_eq!(with_env.app_env(), vec![("K".to_string(), "V".to_string())]);
    plain.cleanup().unwrap();
    with_env.cleanup().unwrap();
}
